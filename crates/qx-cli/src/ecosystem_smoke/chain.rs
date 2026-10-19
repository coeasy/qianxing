use super::{provider_capability, DemoProvider};
use crate::verify_foundation_specs;
use qx_api::{ApiService, ApiState};
use qx_control::{CommandKind, ControlCommand, ControlPlane, Permission};
use qx_core::InstrumentId;
use qx_factor::{
    analyze_factor, CandidateRequest, FactorAnalysisConfig, FactorCatalog, FactorObservation,
    FeatureArtifact, FeatureDefinition,
};
use qx_protocol::{AccountSnapshot, PositionSnapshot};
use qx_provider::{DataKind, DataQuery, ProviderRegistry};
use qx_scheduler::{JobSpec, JobWindow, RetryPolicy, Scheduler, Trigger};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn run_ecosystem_smoke_inner(as_json: bool) -> Result<(), String> {
    macro_rules! println {
        ($($arg:tt)*) => {{ if !as_json { std::println!($($arg)*); } }};
    }
    let instrument = InstrumentId::parse("DEMO.SIM")
        .ok_or_else(|| "生态冒烟示例标的无效: DEMO.SIM".to_string())?;

    let mut factors = FactorCatalog::default();
    factors
        .register_definition(FeatureDefinition {
            name: "momentum".into(),
            version: "v1".into(),
            formula: "close / close[-20] - 1".into(),
            input_fields: vec!["close".into()],
            dependencies: Vec::new(),
            point_in_time: true,
        })
        .map_err(|error| format!("注册因子定义失败: {error:?}"))?;
    factors
        .publish_artifact(FeatureArtifact {
            feature_key: "momentum@v1".into(),
            input_fingerprint: "synthetic-bars".into(),
            as_of: 20,
            coverage_bps: 10_000,
            values: [(instrument.clone(), 123_i128)].into_iter().collect(),
        })
        .map_err(|error| format!("发布因子样本失败: {error:?}"))?;
    let second_instrument = InstrumentId::parse("DEMO2.SIM")
        .ok_or_else(|| "生态冒烟第二示例标的无效: DEMO2.SIM".to_string())?;
    let report = analyze_factor(
        "momentum@v1",
        &[
            FactorObservation {
                timestamp: 1,
                instrument: instrument.clone(),
                factor_value: Some(1),
                forward_returns: [(1, 10)].into_iter().collect(),
                previous_exposure: Some(0),
                target_exposure: Some(100),
                capacity_raw: Some(1_000_000),
                exposures: BTreeMap::new(),
                group_labels: BTreeMap::new(),
            },
            FactorObservation {
                timestamp: 1,
                instrument: second_instrument,
                factor_value: Some(2),
                forward_returns: [(1, 20)].into_iter().collect(),
                previous_exposure: Some(0),
                target_exposure: Some(200),
                capacity_raw: Some(900_000),
                exposures: BTreeMap::new(),
                group_labels: BTreeMap::new(),
            },
        ],
        &FactorAnalysisConfig {
            input_fingerprint: "synthetic-bars".into(),
            ..FactorAnalysisConfig::default()
        },
    )
    .map_err(|error| format!("因子分析失败: {error:?}"))?;
    factors
        .publish_report(report)
        .map_err(|error| format!("发布因子分析报告失败: {error:?}"))?;
    let candidate = factors
        .bind_candidate(CandidateRequest {
            strategy_version: "sma-cross-v1".into(),
            universe_version: "demo-universe-v1".into(),
            parameters: Default::default(),
            data_fingerprint: "synthetic-bars".into(),
            factor_keys: vec!["momentum@v1".into()],
            cost_bps: 8,
            train_start: 1,
            train_end: 100,
            validation_start: 101,
            validation_end: 200,
            intended_exposure: BTreeMap::new(),
            constraints: BTreeMap::new(),
            execution_model: "event-backtest@v1".into(),
            risk_model: "default-risk@v1".into(),
        })
        .map_err(|error| format!("绑定回测候选失败: {error:?}"))?;
    if candidate.event_verified {
        return Err("仅向量候选被错误标记为事件验证".into());
    }
    println!(
        "[因子 · FactorCatalog] feature={} candidate=VectorOnly ✓",
        candidate.config.feature_version
    );

    let mut base = AccountSnapshot::new(1, "main", "default", "SIM", 20);
    base.cash_raw.insert("USD".into(), 100_000);
    base.positions.insert(
        instrument.clone(),
        PositionSnapshot {
            instrument,
            quantity_raw: 10,
            ..PositionSnapshot::default()
        },
    );
    base.seal();
    let mut target = base.clone();
    target.cash_raw.insert("USD".into(), 99_000);
    target.equity_raw = Some(101_000);
    target.seal();
    let diff = base
        .diff(&target)
        .map_err(|error| format!("生成账户快照差异失败: {error:?}"))?;
    let applied = diff
        .apply(&base)
        .map_err(|error| format!("应用账户快照差异失败: {error:?}"))?;
    if applied != target {
        return Err("账户快照差异往返结果不一致".into());
    }
    // 这一段的标题是「QIFI」，所以必须真的走一遍 QIFI 信封的构造→校验→往返，
    // 否则冒烟是在为一份它没碰过的协议作保（V13 R4）。给 `to_qifi`/`QifiEnvelope::{to_json,
    // from_json,validate}` 一个生产（冒烟）调用者，同时让下面这行 ✓ 名副其实。
    let qifi = target.to_qifi();
    qifi.validate()
        .map_err(|error| format!("QIFI 信封校验失败: {error:?}"))?;
    let qifi_json = qifi
        .to_json()
        .map_err(|error| format!("QIFI 信封序列化失败: {error:?}"))?;
    let roundtrip = qx_protocol::QifiEnvelope::from_json(&qifi_json)
        .map_err(|error| format!("QIFI 信封往返解析失败: {error:?}"))?;
    if roundtrip != qifi {
        return Err("QIFI 信封往返结果不一致".into());
    }
    println!(
        "[协议 · QIFI Snapshot] schema=v1 diff_base={:016x} diff_target={:016x} ✓",
        diff.base_state_hash, diff.target_state_hash
    );

    let mut providers = ProviderRegistry::new();
    providers
        .register(Box::new(DemoProvider {
            capability: provider_capability("primary", 1),
            fail: true,
        }))
        .map_err(|error| format!("注册主数据 Provider 失败: {error:?}"))?;
    providers
        .register(Box::new(DemoProvider {
            capability: provider_capability("backup", 2),
            fail: false,
        }))
        .map_err(|error| format!("注册备用数据 Provider 失败: {error:?}"))?;
    let provider_query = DataQuery {
        kind: DataKind::Bar,
        asset_class: "crypto".into(),
        instrument_set: BTreeSet::from(["BTCUSDT.BINANCE".into()]),
        field_set: BTreeSet::new(),
        frequency: "1d".into(),
        adjustment: "none".into(),
        quality_policy: "strict".into(),
        start: 1,
        end: 10,
        as_of: None,
    };
    let result = providers
        .fetch_with_failover(&provider_query)
        .map_err(|error| format!("Provider 故障切换失败: {error:?}"))?;
    if result.retry_chain != ["primary", "backup"] {
        return Err(format!(
            "Provider 故障切换顺序异常: {:?}",
            result.retry_chain
        ));
    }
    let frame = result
        .to_bar_frame(&provider_query)
        .map_err(|error| format!("Provider Bar 帧转换失败: {error:?}"))?;
    if frame.instrument.to_string() != "BTCUSDT.BINANCE" || frame.len() != 1 {
        return Err(format!(
            "Provider Bar 帧内容异常: instrument={} rows={}",
            frame.instrument,
            frame.len()
        ));
    }
    println!(
        "[数据 · ProviderRegistry] provider={} retry_chain={:?} frame_rows={} fingerprint={:016x} ✓",
        result.provider_id,
        result.retry_chain,
        frame.len(),
        frame.digest()
    );

    let mut scheduler = Scheduler::default();
    scheduler
        .register(JobSpec {
            job_id: "load-bars".into(),
            job_version: "v1".into(),
            owner: "research".into(),
            enabled: true,
            trigger: Trigger::TradingCalendar {
                session: "post-close".into(),
            },
            window: JobWindow::PostClose,
            depends_on: vec![],
            input_refs: vec![],
            output_refs: vec!["bars".into()],
            timeout_seconds: 60,
            retry_policy: RetryPolicy::default(),
            concurrency_key: "data".into(),
            idempotency_key: "load-bars-daily".into(),
            permission_scope: "research".into(),
            audit_reason: "ecosystem smoke schedule".into(),
            dry_run: true,
        })
        .map_err(|error| format!("注册调度任务失败: {error:?}"))?;
    let ready_count = scheduler.ready_jobs(&BTreeSet::new()).len();
    if ready_count != 1 {
        return Err(format!(
            "调度就绪任务数量异常: expected=1 actual={ready_count}"
        ));
    }
    println!(
        "[调度 · JobSpec] jobs={} ready=1 deterministic ✓",
        scheduler.len()
    );

    let mut control = ControlPlane::default();
    let audit = control
        .submit(
            ControlCommand {
                command_id: 1,
                request_id: "cli-control-1".into(),
                operator_id: "cli".into(),
                reason: "ecosystem smoke".into(),
                kind: CommandKind::PauseStrategy,
                target: "sma-cross-v1".into(),
                payload: BTreeMap::new(),
                permission: Permission::Trading,
                dry_run: true,
            },
            20,
        )
        .map_err(|error| format!("提交控制命令失败: {error:?}"))?;
    println!(
        "[控制 · ControlCommand] status={:?} digest={:016x} ✓",
        audit.status, audit.command_digest
    );

    let api = ApiService::new(ApiState::default());
    let health_status = api.handle("GET", "/health", "", 20).status;
    let schema_status = api
        .handle("GET", "/schema/account-snapshot-v1", "", 20)
        .status;
    if health_status != 200 || schema_status != 200 {
        return Err(format!(
            "查询 API 冒烟状态异常: health={health_status} schema={schema_status}"
        ));
    }
    println!("[控制 · Query/WebSocket API] health/schema routes ✓");

    // 地基规格对象（qx-spec）：七类声明式文档在本构建里必须既能读入、又能拒绝坏载荷。
    let foundation =
        verify_foundation_specs().map_err(|error| format!("地基规格对象冒烟失败: {error}"))?;
    println!("[规格 · qx-spec] {foundation} ✓");
    Ok(())
}
