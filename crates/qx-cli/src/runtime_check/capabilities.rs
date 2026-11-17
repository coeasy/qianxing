//! Runtime capability matrix and fail-closed readiness evaluation.

use qx_control::CommandKind;
use qx_runtime::{RuntimeConfig, StrategyRuntimeConfig, WorkerRole};
use serde_json::{json, Value};
use std::{collections::BTreeSet, path::Path};

pub(super) fn assess_runtime_capabilities(
    config: &RuntimeConfig,
    runtime_path: &Path,
) -> (Vec<Value>, Vec<Value>) {
    let mut capabilities = Vec::new();
    let mut failures = Vec::new();

    for kind in CommandKind::ALL {
        let id = format!("control.command.{}", command_name(&kind));
        let supported = kind.executed();
        add_capability(
            &mut capabilities,
            &mut failures,
            &id,
            "control_command",
            false,
            if supported {
                "supported"
            } else {
                "unsupported"
            },
            if supported {
                format!("{kind:?} 有生产执行者")
            } else {
                format!("{kind:?} 当前没有生产执行者；命令不得被接受为已执行")
            },
            vec![if supported {
                format!("CommandKind::{kind:?} executor contract")
            } else {
                "实现并验收对应的持久化命令执行者".to_string()
            }],
        );
    }

    let scheduler_enabled = config
        .workers
        .iter()
        .any(|worker| worker.enabled && worker.role == WorkerRole::Scheduler);
    if scheduler_enabled {
        let jobs_path =
            crate::scheduler::scheduler_jobs_path(runtime_path, &config.scheduler.jobs_path);
        match std::fs::read_to_string(&jobs_path) {
            Err(error) => add_capability(
                &mut capabilities,
                &mut failures,
                "scheduler.job_manifest",
                "scheduler_dispatch",
                true,
                "unknown",
                format!(
                    "无法读取启用的 Scheduler 作业清单 {}: {error}",
                    jobs_path.display()
                ),
                vec![format!("可读取的 JobSpec 文件 {}", jobs_path.display())],
            ),
            Ok(payload) => match serde_json::from_str::<Vec<qx_scheduler::JobSpec>>(&payload) {
                Err(error) => add_capability(
                    &mut capabilities,
                    &mut failures,
                    "scheduler.job_manifest",
                    "scheduler_dispatch",
                    true,
                    "unknown",
                    format!("启用的 Scheduler 作业清单无法安全判定: {error}"),
                    vec!["通过当前 JobSpec schema 与派发器形状校验".into()],
                ),
                Ok(jobs) => {
                    for job in jobs.into_iter().filter(|job| job.enabled) {
                        if let Err(error) = job.validate() {
                            let id = format!("scheduler.job.{}", job.job_id);
                            add_capability(
                                &mut capabilities,
                                &mut failures,
                                &id,
                                "scheduler_dispatch",
                                true,
                                "unknown",
                                format!("JobSpec 无效，无法安全判定派发能力: {error:?}"),
                                vec!["通过 JobSpec::validate".into()],
                            );
                            continue;
                        }
                        let reason = qx_scheduler::unsupported_dispatch_shape(&job);
                        let id = format!("scheduler.job.{}", job.job_id);
                        match reason {
                            Some(reason) => add_capability(
                                &mut capabilities,
                                &mut failures,
                                &id,
                                "scheduler_dispatch",
                                true,
                                "unsupported",
                                reason,
                                vec!["trigger=Cron、window=Any、retry_policy.max_attempts=1".into()],
                            ),
                            None => add_capability(
                                &mut capabilities,
                                &mut failures,
                                &id,
                                "scheduler_dispatch",
                                true,
                                "supported",
                                "作业形状由当前 scheduler-worker 派发器支持".into(),
                                vec!["scheduler-worker tick dispatch evidence".into()],
                            ),
                        }
                    }
                }
            },
        }
    }

    let paper_submit_requested = config.environment.eq_ignore_ascii_case("paper")
        && config.workers.iter().any(|worker| {
            worker.enabled
                && matches!(
                    worker.role,
                    WorkerRole::Execution | WorkerRole::SpreadRecovery
                )
        });
    let ashare_strategies = strategies(config)
        .into_iter()
        .filter(|(_, strategy)| declares_ashare_rules(strategy))
        .map(|(label, _)| label)
        .collect::<Vec<_>>();
    let ashare_paper_requested = paper_submit_requested && !ashare_strategies.is_empty();
    let ashare_reason = if ashare_paper_requested {
        format!(
            "{}；受影响配置：{}",
            crate::runtime_wiring::ASHARE_SUBMIT_GAP,
            ashare_strategies.join(", ")
        )
    } else if paper_submit_requested {
        "当前 Paper 提交配置没有声明 A 股规则输入".into()
    } else {
        "只有 Paper 环境启用了 Execution/SpreadRecovery 且策略声明 A 股规则时才适用".into()
    };
    add_capability(
        &mut capabilities,
        &mut failures,
        "paper.ashare_submission",
        "paper_execution",
        ashare_paper_requested,
        if ashare_paper_requested {
            "unsupported"
        } else {
            "not_requested"
        },
        ashare_reason,
        vec![
            "A 股 T+1 当日买入结算状态".into(),
            "按板块与昨收判定整手及涨跌停".into(),
            "提交前账户闸门的 A 股规则验收".into(),
        ],
    );

    // StrategyCapability is a closed serde enum. An unknown value fails RuntimeConfig parsing
    // before this matrix can be returned, so no unknown requirement can be reported as ready.
    for (label, strategy) in strategies(config) {
        let mut requirements = strategy
            .required_capabilities
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        if let Some(kind_name) = strategy.builtin_strategy.as_deref() {
            match qx_strategy::BuiltinStrategyKind::parse(kind_name) {
                Ok(kind) => requirements.extend(kind.required_capabilities()),
                Err(error) => add_capability(
                    &mut capabilities,
                    &mut failures,
                    &format!("strategy.{label}.builtin_kind"),
                    "strategy_host",
                    true,
                    "unknown",
                    format!("无法判定内置策略的固有能力要求: {error}"),
                    vec!["配置受支持的 BuiltinStrategyKind".into()],
                ),
            }
        }
        for required in requirements {
            let id = format!("strategy.{}.{}", label, required.as_str());
            let (status, reason, evidence) = match strategy_host_capabilities(strategy) {
                Some(Ok(host)) => match host.validate_required(&[required]) {
                    Ok(()) => (
                        "supported",
                        format!(
                            "宿主 {} 的静态能力目录支持此要求；worker 启动仍须通过其实际握手",
                            host.host_id
                        ),
                        vec![format!(
                            "运行时宿主 {} 对该要求的启动校验/ACK",
                            host.host_id
                        )],
                    ),
                    Err(error) => (
                        "unsupported",
                        format!("配置的策略宿主不支持此要求: {error}"),
                        vec!["改用支持该能力的策略宿主或移除此要求".into()],
                    ),
                },
                Some(Err(error)) => (
                    "unsupported",
                    format!("无法建立配置策略宿主的能力目录: {error}"),
                    vec!["修复策略宿主类型/数据等级配置并通过宿主能力校验".into()],
                ),
                None => (
                    "unknown",
                    "C ABI 宿主能力取决于实际加载库的导出；runtime-check 未加载策略代码，无法静态确认此要求".into(),
                    vec!["worker 加载受信任 C ABI 库并验证其能力导出".into()],
                ),
            };
            add_capability(
                &mut capabilities,
                &mut failures,
                &id,
                "strategy_host",
                true,
                status,
                reason,
                evidence,
            );
        }
    }

    (capabilities, failures)
}

fn add_capability(
    capabilities: &mut Vec<Value>,
    failures: &mut Vec<Value>,
    id: &str,
    scope: &str,
    requested: bool,
    status: &str,
    reason: String,
    required_evidence: Vec<String>,
) {
    let known_status = matches!(
        status,
        "supported" | "unsupported" | "not_requested" | "unknown"
    );
    let blocking = !known_status || status == "unknown" || (requested && status != "supported");
    let capability_reason = reason.clone();
    let failure_evidence = required_evidence.clone();
    capabilities.push(json!({
        "id": id,
        "scope": scope,
        "status": if known_status { status } else { "unknown" },
        "requested": requested,
        "blocking": blocking,
        "reason": reason,
        "required_evidence": required_evidence,
    }));
    if blocking || status == "unsupported" || !known_status {
        failures.push(json!({
            "capability": id,
            "blocking": blocking,
            "reason": capability_reason,
            "required_evidence": failure_evidence,
        }));
    }
}

fn command_name(kind: &CommandKind) -> String {
    // Keep the diagnostic registry from looking like a command dispatcher to the static
    // executor-coverage audit: enum names are read from Debug rather than re-listed as arms.
    let variant = format!("{kind:?}");
    let mut name = String::with_capacity(variant.len() + 4);
    for (index, character) in variant.chars().enumerate() {
        if character.is_ascii_uppercase() && index > 0 {
            name.push('_');
        }
        name.push(character.to_ascii_lowercase());
    }
    name
}

fn strategies(config: &RuntimeConfig) -> Vec<(String, &StrategyRuntimeConfig)> {
    let mut strategies = vec![("strategy".into(), &config.strategy)];
    strategies.extend(config.strategies.iter().map(|strategy| {
        (
            format!(
                "strategy[{}]",
                strategy.id.as_deref().unwrap_or("<missing-id>")
            ),
            strategy,
        )
    }));
    strategies
}

fn declares_ashare_rules(strategy: &StrategyRuntimeConfig) -> bool {
    strategy.ashare_rules_path.is_some()
        || strategy.ashare_actions_path.is_some()
        || strategy.ashare_calendar_path.is_some()
}

fn strategy_host_capabilities(
    strategy: &StrategyRuntimeConfig,
) -> Option<Result<qx_strategy::capabilities::StrategyHostCapabilitiesV1, String>> {
    use qx_strategy::capabilities::StrategyHostCapabilitiesV1;

    let scoped_account_updates = strategy.account_id.is_some() && strategy.venue_id.is_some();
    if strategy.builtin_strategy.is_some() {
        return Some(Ok(StrategyHostCapabilitiesV1::builtin_rust()));
    }
    if strategy.python_module.is_some() {
        return Some(match strategy.python_strategy_spec.as_ref() {
            Some(spec) => StrategyHostCapabilitiesV1::runtime_python_class(
                spec.get("data_level")
                    .and_then(Value::as_str)
                    .unwrap_or("bar"),
                scoped_account_updates,
            ),
            None => Ok(StrategyHostCapabilitiesV1::runtime_python_module_function(
                scoped_account_updates,
            )),
        });
    }
    if strategy.external_executable.is_some() {
        return Some(Ok(StrategyHostCapabilitiesV1::runtime_external_process(
            scoped_account_updates,
        )));
    }
    if strategy.c_abi_library.is_some() {
        // The catalog depends on the verified exports of the loaded native library.
        return None;
    }
    // A worker without a configured plugin uses the built-in Rust strategy runtime.
    Some(Ok(StrategyHostCapabilitiesV1::builtin_rust()))
}
