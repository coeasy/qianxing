//! 内置确定性演示与自校验：合成 Bar、演示 DataProvider、生态闭环和 Paper 冒烟。
//!
//! 这条链路是 `qx-cli` 无参数运行和 `verify` 自检的底座，不读取运行时配置，
//! 也不连接任何交易所。

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
    pub(crate) total_return: i128,
    pub(crate) max_drawdown: i128,
    pub(crate) final_equity: i128,
}

struct DemoProvider {
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

fn provider_capability(id: &str, priority: u32) -> ProviderCapability {
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
        AccountPositionSnapshot {
            instrument,
            quantity_raw: 10,
            ..AccountPositionSnapshot::default()
        },
    );
    base.seal();
    let mut target = base.clone();
    target.cash_raw.insert("USD".into(), 99_000);
    target.equity_raw = 101_000;
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

/// 双均线策略回测。**关键：bar t 决策 → bar t+1 开盘成交。**
pub(crate) fn run_backtest(bars: &[Bar], seed: u64, fast: usize, slow: usize) -> Outcome {
    let view = DataView::try_new(bars.to_vec(), DataSourceId::new("synthetic"))
        .expect("synthetic bars must pass quality gate");
    let instr = InstrumentId::parse("DEMO.SIM").unwrap();

    let mut matching = BarMatchingEngine::new(
        Box::new(NextBarOpenFillModel),
        ExecutionCostRules::default().fee_model(),
        seed,
    );

    let mut log = EventLog::new();
    let mut analyser = BasicAnalyser::default();
    let mut ledger = Ledger::new();
    let deposit_id = ledger
        .deposit(
            "main",
            "USD",
            Money::from_raw(100_000_000_000_000),
            bars[0].ts,
        )
        .unwrap();
    let mut equity: Vec<i128> = Vec::new();
    let mut next_id: u64 = 1;
    let deposit_entry = ledger
        .entries()
        .iter()
        .find(|entry| entry.id == deposit_id)
        .cloned()
        .expect("initial deposit entry must exist");
    let deposit_seq = log.alloc_seq();
    log.append(Event::new(
        deposit_seq,
        deposit_entry.ts,
        Priority::APPLY,
        EventKind::LedgerApplied {
            entry: deposit_entry,
        },
    ));

    let mut risk = RiskGate::new();
    risk.add(Box::new(MaxQtyRule {
        max_qty: 100_000_000_000, // 100 单位
    }));
    risk.add(Box::new(NoShortRule));
    let mut oms = Oms::new();

    for i in (slow + 1)..bars.len() {
        // 决策只使用截至**上一根** bar 的数据
        let hist = view.as_of(bars[i - 1].ts);
        let pos = ledger.position_for("main", &instr).quantity.raw();
        if hist.len() > slow {
            let n = hist.len();
            let sum = |a: usize, b: usize| hist[a..b].iter().map(|x| x.close).sum::<i128>();
            let fast_now = sum(n - fast, n) / fast as i128;
            let slow_now = sum(n - slow, n) / slow as i128;
            let fast_prev = sum(n - fast - 1, n - 1) / fast as i128;
            let slow_prev = sum(n - slow - 1, n - 1) / slow as i128;

            let golden = fast_prev <= slow_prev && fast_now > slow_now;
            let death = fast_prev >= slow_prev && fast_now < slow_now;

            if golden && pos == 0 {
                let o = mk_order(next_id, &instr, Side::Buy, 10);
                if risk.check(&o, &PositionSnapshot::new(pos, 0)).is_ok() {
                    let _ = oms.submit(o.clone());
                    matching.submit(o);
                    next_id += 1;
                }
            } else if death && pos > 0 {
                let o = mk_order(next_id, &instr, Side::Sell, 10);
                if risk.check(&o, &PositionSnapshot::new(pos, 0)).is_ok() {
                    let _ = oms.submit(o.clone());
                    matching.submit(o);
                    next_id += 1;
                }
            }
        }

        // 本根 bar 开盘撮合上一根 bar 提交的挂单
        let fills = matching.on_bar(&bars[i], bars[i].ts);
        for fl in &fills {
            let order = oms.get(fl.order_id).cloned().unwrap();
            let _ = oms.apply_fill(fl);
            let entry_ids = ledger.apply_fill(&order, fl, "USD").unwrap();
            analyser.on_fill(fl);
            // 必须用 alloc_seq：append 不会推进 seq，否则所有事件 seq 都相同，
            // 事件日志将失去顺序信息（摘要里 seq 恒为 0）。
            let event_seq = log.alloc_seq();
            log.append(Event::new(
                event_seq,
                fl.ts,
                Priority::APPLY,
                EventKind::Filled { fill: fl.clone() },
            ));
            for entry_id in entry_ids {
                let seq = log.alloc_seq();
                let entry = ledger
                    .entries()
                    .iter()
                    .find(|entry| entry.id == entry_id)
                    .cloned()
                    .expect("账簿 entry 必须存在");
                log.append(Event::new(
                    seq,
                    fl.ts,
                    Priority::APPLY,
                    EventKind::LedgerApplied { entry },
                ));
            }
        }

        let mut marks = BTreeMap::new();
        marks.insert(instr.clone(), Price::from_raw(bars[i].close));
        equity.push(ledger.equity_for("main", &marks, "USD").unwrap());
    }

    let m = analyser.report(&equity);
    Outcome {
        hash: log.digest(),
        n_fills: m.n_fills,
        total_fee: m.total_fee,
        total_return: m.total_return,
        max_drawdown: m.max_drawdown,
        final_equity: m.final_equity,
    }
}

pub(crate) fn run_paper_smoke() {
    let instrument = InstrumentId::parse("DEMO.SIM").unwrap();
    let mut venue = PaperVenue::new("paper");
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

pub(crate) fn ecosystem_command(_argv: &[String]) {
    run_ecosystem_smoke();
}

pub(crate) fn paper_command(_argv: &[String]) {
    run_paper_smoke();
}

/// 确定性自校验链路的前三段：质量门 → 双次回测 → 重放哈希比对。
///
/// 返回两个重放结论；`verify` 会再断言一次（与旧实现一致），演示链路只取打印效果。
fn determinism_chain() -> (bool, bool) {
    let bars = gen_bars(400, 20260910);

    // 1. 质量门
    let report = QualityGate::check(&bars);
    println!(
        "[观星 · 质量门] bars={} 判定={:?}",
        bars.len(),
        report.verdict()
    );
    assert_eq!(report.verdict(), Verdict::Ok, "合成数据不应有质量问题");

    // 2. 回测
    let a = run_backtest(&bars, 42, 5, 20);
    let b = run_backtest(&bars, 42, 5, 20); // 完全相同参数
    let c = run_backtest(&bars, 42, 6, 20); // 改一个参数

    println!(
        "\n[星板 · 回测 A] 成交={} 手续费={:.4} 总收益={:.2}% 最大回撤={:.2}% 终值={:.2}",
        a.n_fills,
        f(a.total_fee),
        f(a.total_return) * 100.0,
        f(a.max_drawdown) * 100.0,
        f(a.final_equity)
    );

    let manifest = RunManifest {
        run_id: "cli-demo".into(),
        code_commit: "workspace".into(),
        config_hash: "fast=5;slow=20;seed=42".to_string(),
        data_fingerprint: format!("synthetic:{}", bars.len()),
        input_components: BTreeMap::new(),
        clock_start: bars.first().map(|b| b.ts).unwrap_or(0),
        clock_end: bars.last().map(|b| b.ts).unwrap_or(0),
        global_seed: 42,
        determinism_mode: true,
        result_hash: format!("{:016x}", a.hash),
        strategy_version: "sma-cross-v1".into(),
        instrument_spec_version: "demo-v1".into(),
        model_fingerprint: "next-open+maker-taker".into(),
        input_event_hash: format!("bars:{}", bars.len()),
        output_event_hash: format!("{:016x}", a.hash),
        runtime_version: env!("CARGO_PKG_VERSION").into(),
    };
    println!("[更路 · RunManifest] digest={:016x}", manifest.digest());

    // 3. 三重重放校验（此处验证前两条）
    let same = ReplayVerifier::identical(a.hash, b.hash);
    let changed = ReplayVerifier::changed(a.hash, c.hash);
    println!("\n[更路 · 重放校验]");
    println!("  ① 同输入两次运行哈希一致 : {}", same);
    println!("  ② 改参数后哈希发生变化   : {}", changed);
    assert!(same, "相同输入必须产生相同结果");
    assert!(changed, "修改参数必须改变结果");
    (same, changed)
}

/// `qx-cli verify`：跑到重放校验为止，用作 CI 的确定性门禁。
pub(crate) fn verify_command(_argv: &[String]) {
    let (same, changed) = determinism_chain();
    assert!(same && changed);
}

/// 无参数（或显式 `all`）的完整演示链路：自校验 + 插件清单装配 + Paper 冒烟。
pub(crate) fn demo_command(_argv: &[String]) {
    determinism_chain();
    // 4. 插件清单注册与依赖求解（静态装配计划，不做运行时热插拔）
    println!("\n[卯眼 · 插件清单注册]");
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
    .unwrap();
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
    .unwrap();

    let order = reg.resolve_order().unwrap();
    println!("  插件数={} 加载顺序={:?}", reg.len(), order);
    println!("  独占冲突={:?}", reg.conflicts());

    println!("\n全部自校验通过 ✓");
    run_paper_smoke();
}
