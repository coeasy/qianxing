//! 内置生态 smoke：合成行情、演示数据源与纸面交易的一次性自检链路。
//!
//! 这里的回测只提供**合成输入**，撮合与账本走 `qx-xingban` 真实内核（与 `backtest
//! builtin` 同一装配），CLI 内不再自带第二套撮合引擎。
//!
//! 由 `main.rs` 的 crate 根职责簇拆出（Phase 4p），条目经根部的
//! `pub(crate) use ecosystem_smoke::*;` 再导出。

use super::*;

/// 定点 → f64，仅用于打印。
pub(crate) fn f(x: i128) -> f64 {
    x as f64 / 1e9
}

/// 生成确定性合成行情（同一 seed 必然产生同一序列）。
pub(crate) fn gen_bars(n: usize, seed: u64) -> Vec<Bar> {
    let mut rng = DeterministicRng::new(seed);
    let mut bars = Vec::with_capacity(n);
    let mut px: i128 = 100_000_000_000; // 100.0

    for i in 0..n {
        let drift = ((rng.next_u64() % 2001) as i128) - 1000;
        px = (px + drift * 1_000_000).max(1_000_000_000);
        let open = px;
        let close = px + (((rng.next_u64() % 1001) as i128) - 500) * 1_000_000;
        let high = open.max(close) + ((rng.next_u64() % 501) as i128) * 1_000_000;
        let low = (open.min(close) - ((rng.next_u64() % 501) as i128) * 1_000_000).max(1_000_000);
        let volume = 1_000 + (rng.next_u64() % 5_000) as i128;
        bars.push(Bar::new(
            (i as u64 + 1) * 1_000_000_000,
            open,
            high,
            low,
            close,
            volume,
        ));
    }
    bars
}

pub(crate) struct Outcome {
    pub(crate) hash: u64,
    pub(crate) n_fills: usize,
    pub(crate) total_fee: i128,
    pub(crate) return_bps: i32,
    pub(crate) max_drawdown_bps: u32,
    pub(crate) final_equity: i128,
}

pub(crate) struct DemoProvider {
    capability: ProviderCapability,
    fail: bool,
}

impl DataProvider for DemoProvider {
    fn capability(&self) -> &ProviderCapability {
        &self.capability
    }

    fn fetch(&self, _query: &DataQuery) -> Result<ProviderResult, ProviderError> {
        if self.fail {
            return Err(ProviderError::new(
                ProviderErrorClass::SwitchProvider,
                "demo primary unavailable",
            ));
        }
        let mut result = ProviderResult {
            records: vec![qx_guanxing::RawRecord {
                source: DataSourceId::new(self.capability.provider_id.clone()),
                event_time: 1,
                receive_time: 2,
                payload_hash: 3,
                schema_version: 1,
            }],
            provider_id: self.capability.provider_id.clone(),
            provider_version: self.capability.version.clone(),
            request_id: "cli-ecosystem".into(),
            retry_chain: Vec::new(),
            received_at: 2,
            source_hash: 0,
        };
        result.source_hash = result.compute_source_hash();
        Ok(result)
    }
}

pub(crate) fn provider_capability(id: &str, priority: u32) -> ProviderCapability {
    ProviderCapability {
        provider_id: id.into(),
        version: "v1".into(),
        data_kinds: [DataKind::Bar].into_iter().collect(),
        asset_classes: ["crypto".into()].into_iter().collect(),
        frequencies: ["1d".into()].into_iter().collect(),
        auth_scope: "public".into(),
        rate_limit_per_second: 10,
        freshness_seconds: 60,
        historical_start: 0,
        historical_end: u64::MAX,
        realtime: false,
        priority,
        quality_score: 100,
        cost_score: 1,
    }
}

pub(crate) fn run_ecosystem_smoke() {
    let instrument = InstrumentId::parse("DEMO.SIM").unwrap();

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
        .unwrap();
    factors
        .publish_artifact(FeatureArtifact {
            feature_key: "momentum@v1".into(),
            input_fingerprint: "synthetic-bars".into(),
            as_of: 20,
            coverage_bps: 10_000,
            values: [(instrument.clone(), 123_i128)].into_iter().collect(),
        })
        .unwrap();
    let second_instrument = InstrumentId::parse("DEMO2.SIM").unwrap();
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
    .unwrap();
    factors.publish_report(report).unwrap();
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
        .unwrap();
    assert!(!candidate.event_verified);
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
    let diff = base.diff(&target).unwrap();
    assert_eq!(diff.apply(&base).unwrap(), target);
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
        .unwrap();
    providers
        .register(Box::new(DemoProvider {
            capability: provider_capability("backup", 2),
            fail: false,
        }))
        .unwrap();
    let result = providers
        .fetch_with_failover(&DataQuery {
            kind: DataKind::Bar,
            asset_class: "crypto".into(),
            instrument_set: BTreeSet::new(),
            field_set: BTreeSet::new(),
            frequency: "1d".into(),
            adjustment: "none".into(),
            quality_policy: "strict".into(),
            start: 1,
            end: 10,
            as_of: None,
        })
        .unwrap();
    assert_eq!(result.retry_chain, ["primary", "backup"]);
    println!(
        "[数据 · ProviderRegistry] provider={} retry_chain={:?} ✓",
        result.provider_id, result.retry_chain
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
        .unwrap();
    assert_eq!(scheduler.ready_jobs(&BTreeSet::new()).len(), 1);
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
        .unwrap();
    println!(
        "[控制 · ControlCommand] status={:?} digest={:016x} ✓",
        audit.status, audit.command_digest
    );

    let api = ApiService::new(ApiState::default());
    assert_eq!(api.handle("GET", "/health", "", 20).status, 200);
    assert_eq!(
        api.handle("GET", "/schema/account-snapshot-v1", "", 20)
            .status,
        200
    );
    println!("[控制 · Query/WebSocket API] health/schema routes ✓");
}

pub(crate) fn mk_order(id: u64, instr: &InstrumentId, side: Side, qty: i64) -> Order {
    Order {
        client_id: id,
        instrument: instr.clone(),
        side,
        qty: Quantity::from_i64(qty),
        limit: None,
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
    }
}

/// 合成行情 + **真实撮合内核**的双均线回测产物。
///
/// 引擎与 `backtest builtin` 同源：装配走 `BarBacktestAssembly`（含 next-open 成交与
/// maker/taker 2/5bp 费率），策略走内置 `sma_cross`。本文件不再自带第二套撮合循环
/// （V10 §4.3 第 3 项）；只有输入序列是合成的。
pub(crate) fn run_backtest(bars: &[Bar], seed: u64, fast: usize, slow: usize) -> Outcome {
    let instrument = InstrumentId::parse("DEMO.SIM").unwrap();
    let strategy_config = BuiltinStrategyConfig {
        fast_window: fast,
        slow_window: slow,
        ..BuiltinStrategyConfig::new(
            BuiltinStrategyKind::SmaCross,
            "cli-selfcheck-sma",
            instrument.clone(),
            Quantity::from_i64(10),
        )
        .expect("内置双均线默认参数必须合法")
    };
    let context = NativeStrategyContext {
        strategy_id: "cli-selfcheck-sma".into(),
        strategy_version: format!("builtin-{}-v1", BuiltinStrategyKind::SmaCross.name()),
        account_id: "main".into(),
        venue_id: instrument.venue.to_string(),
        data_fingerprint: format!("synthetic:{}", bars.len()),
        as_of: bars.first().map(|bar| bar.ts).unwrap_or(1),
        positions: BTreeMap::new(),
        cash: BTreeMap::from([("USDT".into(), Money::from_i64(100_000).raw())]),
        available_margin_raw: Some(Money::from_i64(100_000).raw()),
        risk_state: "selfcheck".into(),
    };
    let report = run_builtin_strategy_on_bars(
        // 自检不读运行时配置：它证明的是接线，口径固定为内核默认。
        BarBacktestAssembly::new(
            &instrument,
            "main",
            seed,
            &default_execution_cost_binding(),
            // 同上：撮合口径固定为内核默认，自检不替使用者选一个更乐观的成交价。
            bar_fill_model(None, None).expect("内核默认撮合模型不需要 market spec"),
        )
        .into_config(),
        strategy_config,
        context,
        bars,
    )
    .expect("合成输入的真实内核回测必须跑通");
    Outcome {
        hash: report.result_hash(),
        n_fills: report.fills.len(),
        total_fee: report.fees_raw,
        return_bps: report.return_bps,
        max_drawdown_bps: report.max_drawdown_bps,
        final_equity: report.final_equity(),
    }
}

pub(crate) fn run_paper_smoke() {
    let instrument = InstrumentId::parse("DEMO.SIM").unwrap();
    let mut venue = PaperVenue::new("paper", default_execution_cost_binding().fee_model());
    let order = mk_order(9001, &instrument, Side::Buy, 2);
    let accepted = venue.submit(order, 1).unwrap();
    assert!(matches!(accepted.as_slice(), [VenueEvent::Accepted { .. }]));
    let events = venue.on_quote(
        &instrument,
        QuoteTick::new(
            2,
            Price::from_i64(99),
            Quantity::from_i64(10),
            Price::from_i64(100),
            Quantity::from_i64(10),
            1,
        ),
    );
    assert!(matches!(events.as_slice(), [VenueEvent::Fill(_)]));
    assert_eq!(venue.snapshot()[0].status, OrderStatus::Filled);

    let remote_snapshot = venue.snapshot();
    venue.disconnect();
    assert!(venue
        .submit(mk_order(9002, &instrument, Side::Buy, 1), 3)
        .is_err());
    assert_eq!(venue.state(), qx_zhenlu::ConnectorState::ReconcileRequired);
    venue.reconnect();
    assert!(venue.connected());
    assert!(venue
        .reconcile_snapshot(&remote_snapshot)
        .unwrap()
        .is_empty());
    println!("[针路 · PaperVenue] 订单接受/报价成交/断线转对账/恢复通过 ✓");
}
