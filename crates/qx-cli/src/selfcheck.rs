//! 确定性自校验演示：同输入同哈希、改参数变哈希、无前视偏差、插件装配顺序。
//!
//! `all` / `verify` 两个入口命令复用这一条链路，进程内的断言即 CLI 冒烟测试。
//! 输入序列是合成的（`gen_bars`），但撮合与账本走 `qx-xingban` 真实回测内核；
//! 因此下面每一行产物都显式标注 `DEMO 合成输入`，不让人误读成真实行情上的结果。

use crate::build_identity;
use crate::{
    collect_doctor_report, collect_live_check_report, collect_paper_check_report,
    collect_runtime_check_report, f, gen_bars, print_json_report, repository_deploy_path,
    run_backtest, run_paper_smoke, unified_default_runtime_path, BAR_MATCHING_KERNEL,
};
use qx_core::{ReplayVerifier, RunManifest};
use qx_guanxing::{QualityGate, Verdict};
use qx_plugin::{Cardinality, Manifest, Provides, Registry, POINT_FEE_MODEL, POINT_MATCHER};
use qx_strategy::BuiltinStrategyKind;
use std::collections::BTreeMap;

/// 自校验深度由 `cli.rs` 这一个分派点决定，本模块不再自行比较命令名。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scope {
    /// `verify`：只校验确定性内核（质量门、重放哈希），不触发插件装配与 Paper 冒烟。
    KernelOnly,
    /// `all`：完整链路，含插件装配与 Paper 主链路冒烟。
    Full,
}

pub(crate) fn run(scope: Scope, as_json: bool) -> Result<(), String> {
    if scope == Scope::Full {
        let report = collect_full_report();
        if as_json {
            print_json_report(&report)?;
        } else {
            print_full_summary(&report);
        }
        if report.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            std::process::exit(1);
        }
        return Ok(());
    }
    if as_json {
        let report = collect_report(scope);
        print_json_report(&report)?;
        if report.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            std::process::exit(1);
        }
        return Ok(());
    }
    run_inner(scope, false)
}

fn print_full_summary(report: &serde_json::Value) {
    println!(
        "[全量体检] status={}",
        if report["ok"] == true {
            "pass"
        } else {
            "blocked"
        }
    );
    for layer_name in ["R", "P", "O", "L"] {
        let layer = &report["readiness_layers"][layer_name];
        println!(
            "[就绪层 {layer_name}] status={} detail={}",
            layer["status"].as_str().unwrap_or("not_assessed"),
            layer["detail"].as_str().unwrap_or("详见 components 报告")
        );
    }
    for failure in report["failures"].as_array().into_iter().flatten() {
        println!(
            "[全量体检 · 阻断] {}",
            failure.as_str().unwrap_or("检查失败")
        );
    }
    for warning in report["warnings"].as_array().into_iter().flatten() {
        println!(
            "[全量体检 · 提示] {}",
            warning.as_str().unwrap_or("检查告警")
        );
    }
}

/// `all --json` is the single orchestrated R/P/O/L health report. The component
/// collectors retain ownership of their checks; this layer only combines evidence.
pub(crate) fn collect_full_report() -> serde_json::Value {
    let selfcheck = collect_report(Scope::Full);
    let runtime_path = unified_default_runtime_path();
    let paper_path = repository_deploy_path("qianxing.runtime.paper-strategy.example.json");
    let live_path = repository_deploy_path("qianxing.runtime.production.example.json");
    let doctor = collect_doctor_report(&runtime_path)
        .unwrap_or_else(|error| failed_component("doctor", &error));
    let runtime = collect_runtime_check_report(&runtime_path)
        .unwrap_or_else(|error| failed_component("runtime-check", &error));
    let paper = collect_paper_check_report(&paper_path)
        .unwrap_or_else(|error| failed_component("paper-check", &error));
    let live = collect_live_check_report(&live_path)
        .unwrap_or_else(|error| failed_component("live-check", &error));

    let mut layers = serde_json::Map::new();
    layers.insert(
        "R".into(),
        combine_layers(&[layer(&doctor, "R"), layer(&selfcheck, "R")]),
    );
    layers.insert("P".into(), layer(&paper, "P"));
    layers.insert("O".into(), layer(&runtime, "O"));
    layers.insert("L".into(), layer(&live, "L"));
    let readiness_layers = serde_json::Value::Object(layers);
    let failed = readiness_layers
        .as_object()
        .is_some_and(|values| values.values().any(|value| value["status"] == "blocked"));
    let checks = [
        ("selfcheck", &selfcheck),
        ("doctor", &doctor),
        ("runtime_check", &runtime),
        ("paper_check", &paper),
        ("live_check", &live),
    ]
    .into_iter()
    .map(|(name, report)| {
        serde_json::json!({
            "name": name,
            "ok": report.get("ok").and_then(serde_json::Value::as_bool).unwrap_or(false),
        })
    })
    .collect::<Vec<_>>();
    let warnings = [&doctor, &runtime, &paper, &live]
        .into_iter()
        .flat_map(|report| {
            report
                .get("warnings")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    let failures = [&selfcheck, &doctor, &runtime, &paper, &live]
        .into_iter()
        .flat_map(|report| {
            report
                .get("failures")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "schema_version": 1,
        "command": "all",
        "ok": !failed,
        "readiness_layers": readiness_layers,
        "checks": checks,
        "warnings": warnings,
        "failures": failures,
        "reports": {
            "selfcheck": selfcheck,
            "doctor": doctor,
            "runtime_check": runtime,
            "paper_check": paper,
            "live_check": live,
        },
        "network_accessed": false,
        "orders_sent": false
    })
}

fn layer(report: &serde_json::Value, name: &str) -> serde_json::Value {
    report.get("readiness_layers")
        .and_then(|layers| layers.get(name))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"status":"not_assessed","scope":"missing_evidence","evidence":[]}))
}

fn combine_layers(values: &[serde_json::Value]) -> serde_json::Value {
    let status = if values.iter().any(|value| value["status"] == "blocked") {
        "blocked"
    } else if values.iter().all(|value| value["status"] == "pass") {
        "pass"
    } else {
        "not_assessed"
    };
    serde_json::json!({
        "status": status,
        "scope": "combined_component_evidence",
        "components": values,
    })
}

fn failed_component(command: &str, error: &str) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "command": command,
        "ok": false,
        "readiness_layers": {
            "R": {"status":"blocked","scope":"component_error","evidence":[]},
            "P": {"status":"blocked","scope":"component_error","evidence":[]},
            "O": {"status":"blocked","scope":"component_error","evidence":[]},
            "L": {"status":"blocked","scope":"component_error","evidence":[]}
        },
        "warnings": [],
        "failures": [format!("{command}: {error}")],
        "error": error,
    })
}

pub(crate) fn collect_report(scope: Scope) -> serde_json::Value {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_inner(scope, true)));
    let failure = match result {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error),
        Err(_) => Some("自校验内部发生 panic".to_string()),
    };
    let succeeded = failure.is_none();
    let full = scope == Scope::Full;
    let mut checks = vec![serde_json::json!({
        "name": "selfcheck_chain",
        "status": if succeeded { "pass" } else { "blocked" },
        "scope": if full { "kernel_plugin" } else { "kernel_only" }
    })];
    if full && succeeded {
        checks.push(serde_json::json!({
            "name": "paper_venue_smoke",
            "status": "pass",
            "scope": "local_synthetic_venue_only"
        }));
    }
    serde_json::json!({
        "schema_version": 2,
        "command": if full { "all" } else { "verify" },
        "ok": succeeded,
        "readiness_layers": {
            "R": {"status": if succeeded { "pass" } else { "blocked" }, "scope": "synthetic_selfcheck", "evidence": if succeeded { vec!["quality_gate", "deterministic_backtest_replay", "run_manifest"] } else { Vec::<&str>::new() }, "detail": "合成输入上的确定性内核自校验；不代表真实行情表现"},
            "P": {"status": "not_assessed", "scope": "paper_execution", "evidence": Vec::<&str>::new(), "detail": if full && succeeded { "仅执行 PaperVenue 本地合成订单生命周期冒烟，没有执行 paper-check 主链路" } else { "本次没有确认完成 Paper 主链路执行"}},
            "O": {"status": "not_assessed", "scope": "operator_runtime", "evidence": Vec::<&str>::new(), "detail": "未启动或检查运行时 worker"},
            "L": {"status": "not_assessed", "scope": "live_account", "evidence": Vec::<&str>::new(), "detail": "未连接真实账户或提交真实订单"}
        },
        "checks": checks,
        "warnings": [],
        "failures": failure.into_iter().collect::<Vec<_>>(),
        "network_accessed": false,
        "orders_sent": false
    })
}

fn run_inner(scope: Scope, as_json: bool) -> Result<(), String> {
    macro_rules! println {
        ($($arg:tt)*) => {{ if !as_json { std::println!($($arg)*); } }};
    }
    let bars = gen_bars(400, 20260910);

    // 1. 质量门
    let report = QualityGate::check(&bars);
    println!(
        "[观星 · 质量门 · DEMO 合成输入] bars={} 判定={:?}",
        bars.len(),
        report.verdict()
    );
    assert_eq!(report.verdict(), Verdict::Ok, "合成数据不应有质量问题");

    // 2. 回测：真实内核（与 `backtest builtin` 同一装配）跑合成输入
    let a = run_backtest(&bars, 42, 5, 20)?;
    let b = run_backtest(&bars, 42, 5, 20)?; // 完全相同参数
    let c = run_backtest(&bars, 42, 6, 20)?; // 改一个参数

    println!();
    println!(
        "[星板 · 回测 A · DEMO 合成输入] 内核={} 成交={} 手续费={:.4} 总收益={:.2}% 最大回撤={:.2}% 终值={:.2}",
        BAR_MATCHING_KERNEL,
        a.n_fills,
        f(a.total_fee),
        a.return_bps as f64 / 100.0,
        a.max_drawdown_bps as f64 / 100.0,
        f(a.final_equity)
    );

    let manifest = RunManifest {
        run_id: "cli-demo".into(),
        code_commit: build_identity::BUILD_REVISION.into(),
        config_hash: "fast=5;slow=20;seed=42".to_string(),
        data_fingerprint: format!("synthetic:{}", bars.len()),
        input_components: BTreeMap::new(),
        clock_start: bars.first().map(|b| b.ts).unwrap_or(0),
        clock_end: bars.last().map(|b| b.ts).unwrap_or(0),
        global_seed: 42,
        determinism_mode: true,
        result_hash: format!("{:016x}", a.hash),
        strategy_version: format!("builtin-{}-v1", BuiltinStrategyKind::SmaCross.name()),
        instrument_spec_version: "demo-v1".into(),
        model_fingerprint: "next-open+maker-taker".into(),
        input_event_hash: format!("bars:{}", bars.len()),
        output_event_hash: format!("{:016x}", a.hash),
        runtime_version: build_identity::RUNTIME_VERSION.into(),
        canonical_run_spec_json: None,
        canonical_run_spec_fingerprint: None,
    };
    println!(
        "[更路 · RunManifest · DEMO 合成输入] digest={:016x}",
        manifest.digest()
    );

    // 3. 三重重放校验（此处验证前两条）
    let same = ReplayVerifier::identical(a.hash, b.hash);
    let changed = ReplayVerifier::changed(a.hash, c.hash);
    println!();
    println!("[更路 · 重放校验]");
    println!("[更路 · 重放校验] ① 同输入两次运行哈希一致 : {}", same);
    println!("[更路 · 重放校验] ② 改参数后哈希发生变化   : {}", changed);
    if !same {
        return Err("相同输入的两次自检回测结果哈希不一致".into());
    }
    if !changed {
        return Err("改变策略参数后自检回测结果哈希未变化".into());
    }

    if scope == Scope::KernelOnly {
        return Ok(());
    }

    // 4. 插件装配
    println!();
    println!("[卯眼 · 插件装配]");
    let mut reg = Registry::new();
    reg.register(
        Manifest {
            id: "sys.simulation".into(),
            version: "0.1.0".into(),
            kind: "domain-mod".into(),
            provides: vec![Provides {
                point: POINT_MATCHER.into(),
                cardinality: Cardinality::Exclusive,
                priority: 100,
            }],
            requires: vec![],
            replaces: vec![],
            capabilities: vec!["simulation".into()],
            permissions: vec![],
            healthcheck_timeout_ms: 1000,
            shutdown_timeout_ms: 1000,
            config_schema: "{}".into(),
            manifest_hash: 0,
            signature: None,
        }
        .sign(),
    )
    .map_err(|error| format!("注册 sys.simulation 插件失败: {error:?}"))?;
    reg.register(
        Manifest {
            id: "sys.transaction-cost".into(),
            version: "0.1.0".into(),
            kind: "domain-mod".into(),
            provides: vec![Provides {
                point: POINT_FEE_MODEL.into(),
                cardinality: Cardinality::Exclusive,
                priority: 100,
            }],
            requires: vec!["sys.simulation".into()],
            replaces: vec![],
            capabilities: vec!["fee".into()],
            permissions: vec![],
            healthcheck_timeout_ms: 1000,
            shutdown_timeout_ms: 1000,
            config_schema: "{}".into(),
            manifest_hash: 0,
            signature: None,
        }
        .sign(),
    )
    .map_err(|error| format!("注册 sys.transaction-cost 插件失败: {error:?}"))?;

    let order = reg
        .resolve_order()
        .map_err(|error| format!("解析插件装配顺序失败: {error:?}"))?;
    println!(
        "[卯眼 · 插件装配] 插件数={} 加载顺序={:?}",
        reg.len(),
        order
    );
    println!("[卯眼 · 插件装配] 独占冲突={:?}", reg.conflicts());

    println!("[观星 · 自校验] 全部自校验通过 ✓");
    run_paper_smoke(!as_json)?;
    Ok(())
}
