//! 策略与 Paper worker 入口的行为用例：从运行时配置读到一次真实调用，不经过回测链。
//!
//! 这些用例原先住在 `backtest_entries.rs`：它们验的是 worker 侧的入口，与 Bar 回测
//! 装配无关，Phase 4o 的主题目录按入口所在分文件。

use super::*;

struct TemporaryStrategyProbeModule(std::path::PathBuf);

impl Drop for TemporaryStrategyProbeModule {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn builtin_strategy_worker_path_reads_bar_snapshot() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-builtin-worker-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.builtin-strategy.example.json");
    let mut config = read_runtime_config(&runtime).unwrap();
    config.storage.data_dir = root.to_string_lossy().into_owned();
    resolve_strategy_runtime_paths(&mut config.strategy, &runtime);
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let output = invoke_builtin_strategy(
        &root,
        &config,
        None,
        &instrument,
        "builtin-worker-test",
        u64::MAX,
    )
    .unwrap();
    assert_eq!(output.request_id, "builtin-worker-test");
    assert_eq!(output.strategy_id, "strategy-builtin");
    assert_eq!(output.instrument, "BTCUSDT.BINANCE");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn builtin_worker_reads_the_declared_signal_parameters() {
    // 交易链路（paper/live）与四条回测链共用同一份声明式信号口径（V11 Q64）。
    // 注意不能拿 `qianxing.runtime.builtin-strategy.example.json` 里的那四项做基线：
    // 它写的正好是 5/20/14/100，与写死默认一模一样，换了也证明不了什么 —— 逐项自己给值。
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.builtin-strategy.example.json");
    let base = read_runtime_config(&runtime).unwrap();
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let windows = |value: &qx_strategy::BuiltinStrategyConfig| {
        (
            value.fast_window,
            value.slow_window,
            value.period,
            value.threshold_bps,
        )
    };
    let mut declared = base.clone();
    declared.strategy.builtin_fast_window = Some(2);
    declared.strategy.builtin_slow_window = Some(3);
    declared.strategy.builtin_period = Some(9);
    declared.strategy.builtin_threshold_bps = Some(50);
    let assembled =
        qx_runtime::builtin_strategy_config_from_runtime(&declared.strategy, &instrument).unwrap();
    assert_eq!(
        windows(&assembled),
        (2, 3, 9, 50),
        "声明的四项信号参数没有全部进装配"
    );
    let mut cleared = base.clone();
    cleared.strategy.builtin_fast_window = None;
    cleared.strategy.builtin_slow_window = None;
    cleared.strategy.builtin_period = None;
    cleared.strategy.builtin_threshold_bps = None;
    let fallback =
        qx_runtime::builtin_strategy_config_from_runtime(&cleared.strategy, &instrument).unwrap();
    assert_eq!(
        windows(&fallback),
        (5, 20, 14, 100),
        "四项全缺时才该回到写死默认"
    );

    // 只给单边窗口时，运行时体检看不到（它只比两个都写了的键）；非法组合要等这项并进
    // 默认慢窗之后才成立，所以装配尾部必须复检。体检按 kind 的清单做（V12 #102）：
    // 例子里的 macd 一个窗口都不读，25/20 那种"看着非法"的组合对它既不改结果、也不该拒。
    let mut one_sided = declared.clone();
    one_sided.strategy.builtin_strategy = Some("ema_cross".into());
    one_sided.strategy.builtin_slow_window = None;
    one_sided.strategy.builtin_fast_window = Some(25);
    let error = qx_runtime::builtin_strategy_config_from_runtime(&one_sided.strategy, &instrument)
        .unwrap_err();
    assert!(
        error.contains("内置策略参数非法"),
        "单边窗口的非法组合没有被复检: {error}"
    );
    let mut unused_illigal = declared.clone();
    unused_illigal.strategy.builtin_fast_window = Some(9);
    unused_illigal.strategy.builtin_slow_window = Some(3);
    let ignored =
        qx_runtime::builtin_strategy_config_from_runtime(&unused_illigal.strategy, &instrument)
            .unwrap_or_else(|error| {
                panic!("macd 不读快慢窗口，9/3 这种清单外取值不许拒一轮交易: {error}")
            });
    assert_eq!(
        windows(&ignored),
        (9, 3, 9, 50),
        "清单外的取值仍要原样装配（播报才写得出 declared_unused），只是不参与信号"
    );
}

#[test]
fn builtin_worker_receives_shared_rust_sizing_parameters() {
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.builtin-strategy.example.json");
    let mut config = read_runtime_config(&runtime).unwrap();
    config.strategy.builtin_target_notional_raw = Some(123_000);
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let assembled =
        qx_runtime::builtin_strategy_config_from_runtime(&config.strategy, &instrument).unwrap();
    assert_eq!(assembled.target_notional_raw, Some(123_000));

    config.strategy.builtin_target_notional_raw = None;
    config.strategy.builtin_target_available_margin_bps = Some(2_500);
    let assembled =
        qx_runtime::builtin_strategy_config_from_runtime(&config.strategy, &instrument).unwrap();
    assert_eq!(assembled.target_available_margin_bps, Some(2_500));

    config.strategy.builtin_target_available_margin_bps = None;
    config.strategy.builtin_target_volatility_bps = Some(750);
    let assembled =
        qx_runtime::builtin_strategy_config_from_runtime(&config.strategy, &instrument).unwrap();
    assert_eq!(assembled.target_volatility_bps, Some(750));

    config.strategy.builtin_target_volatility_bps = None;
    config.strategy.builtin_target_kelly_fraction_bps = Some(5_000);
    config.strategy.builtin_kelly_lookback_bars = Some(60);
    let assembled =
        qx_runtime::builtin_strategy_config_from_runtime(&config.strategy, &instrument).unwrap();
    assert_eq!(assembled.target_kelly_fraction_bps, Some(5_000));
    assert_eq!(assembled.kelly_lookback_bars, Some(60));
}

#[test]
fn paper_builtin_kelly_sizing_reads_replayed_buying_power_and_bars() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let root = workspace_root.join(".task-tmp").join(format!(
        "paper-kelly-worker-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let template = workspace_root
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    config.strategy.id = Some("strategy-kelly-paper".into());
    config.strategy.version = "paper-kelly-e2e-v1".into();
    config.strategy.instrument = Some("BTCUSDT.BINANCE".into());
    config.strategy.builtin_strategy = Some("momentum".into());
    config.strategy.builtin_quantity = Some(1_000_000_000);
    config.strategy.builtin_target_kelly_fraction_bps = Some(5_000);
    config.strategy.builtin_kelly_lookback_bars = Some(30);
    let bars_path = root.join("rising-bars.json");
    let closes = (0..70_i128)
        .map(|index| (100 + index - if index % 5 == 0 { 2 } else { 0 }) * SCALE)
        .collect::<Vec<_>>();
    let bars = serde_json::json!({
        "instrument": "BTCUSDT.BINANCE",
        "source": "paper-kelly-test-v1",
        "ts": (1..=70_u64).collect::<Vec<_>>(),
        "open_raw": closes,
        "high_raw": closes.iter().map(|close| close + SCALE).collect::<Vec<_>>(),
        "low_raw": closes.iter().map(|close| close - SCALE).collect::<Vec<_>>(),
        "close_raw": closes,
        "volume_raw": vec![10 * SCALE; 70],
    });
    std::fs::write(&bars_path, serde_json::to_vec(&bars).unwrap()).unwrap();
    config.strategy.bars_snapshot_path = Some(bars_path.to_string_lossy().into_owned());
    let spec_path = workspace_root
        .join("deploy")
        .join("qianxing.binance.spot.spec.json");
    config
        .workers
        .iter_mut()
        .find(|worker| worker.id == "paper-execution")
        .expect("paper sample missing execution worker")
        .instrument_spec_path = Some(spec_path.to_string_lossy().into_owned());
    let runtime_path = root.join("runtime.json");
    std::fs::write(&runtime_path, config.to_json().unwrap()).unwrap();

    // The strategy must size from replayed Paper cash and the execution worker's matching spec.
    let mut pipeline = LiveEventPipeline::open(&data_dir, paper_account_log(), "USDT").unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountCashflow {
                cashflow: AccountCashflow {
                    account_id: "main".into(),
                    venue_id: "paper".into(),
                    currency: "USDT".into(),
                    kind: CashflowKind::Transfer,
                    amount: Money::from_i64(10_000),
                    external_id: "kelly-paper:initial-cash".into(),
                },
            },
            1,
            1,
            1,
            "kelly-paper:initial-cash",
        ))
        .unwrap();
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let input = build_strategy_contract_input(
        &data_dir,
        &config,
        &instrument,
        "kelly-paper-context",
        u64::MAX,
    )
    .unwrap();
    assert!(
        input.risk_state.contains("paper-market-spec-buying-power"),
        "Kelly sizing must report its Paper buying-power source: {}",
        input.risk_state
    );
    assert!(input.available_margin_raw.unwrap_or_default() > 0);
    let output = invoke_builtin_strategy(
        &data_dir,
        &config,
        None,
        &instrument,
        "kelly-paper-worker",
        u64::MAX,
    )
    .unwrap();
    assert!(
        output.intents.iter().any(|intent| intent.qty_raw > 0),
        "positive momentum and positive replayed buying power should produce a sized intent: {output:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_runtime_resolves_execution_market_specs_from_config_directory() {
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&runtime).unwrap();
    let worker = config
        .workers
        .iter_mut()
        .find(|worker| worker.instrument_spec_path.is_some())
        .unwrap();
    let expected =
        resolve_runtime_relative_path(&runtime, worker.instrument_spec_path.as_deref().unwrap());
    resolve_worker_runtime_paths(worker, &runtime);
    assert_eq!(
        PathBuf::from(worker.instrument_spec_path.as_deref().unwrap()),
        expected
    );
}

#[test]
fn strategy_worker_rejects_lost_lease_before_cursor_commit_and_replays_updates() {
    use qx_core::{Fill, OrderTrace, Price, Quantity};
    use qx_datastruct::BarFrame;

    let root = temp_cli_case_dir("strategy-execution-updates-worker");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut config = paper_runtime_config(&data_dir);
    config.strategy.id = Some("strategy-execution-updates-probe".into());
    config.strategy.version = "strategy-execution-updates-probe-v1".into();
    config.strategy.instrument = Some("BTCUSDT.BINANCE".into());
    config.strategy.live_enabled = true;
    config.strategy.live_timeframe = "1m".into();
    config.strategy.live_closed_only = false;
    config.strategy.python_timeout_ms = 30_000;
    config.workers.push(qx_runtime::WorkerConfig {
        id: "strategy-probe-market".into(),
        role: WorkerRole::MarketData,
        enabled: true,
        account_id: None,
        venue_id: Some("BINANCE".into()),
        endpoint: Some("https://example.test".into()),
        symbols: vec!["BTCUSDT.BINANCE".into()],
        settlement_currency: None,
        credential_env: None,
        credential_files: None,
        instrument_spec_path: None,
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    });

    let probe_nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let module_name = format!(
        "strategy_execution_probe_{}_{}",
        std::process::id(),
        probe_nonce
    );
    let module_dir = workspace_root.join("python");
    let module_path = module_dir.join(format!("{module_name}.py"));
    let _module_guard = TemporaryStrategyProbeModule(module_path.clone());
    let mode_path = root.join("probe-mode.txt");
    let trace_path = root.join("probe-calls.txt");
    let queue_root = data_dir.join(&config.scheduler.job_queue_path);
    let ready_path = root.join("probe-ready.txt");
    let release_path = root.join("probe-release.txt");
    std::fs::write(&mode_path, "fail").unwrap();
    let python_path_literal =
        |path: &Path| serde_json::to_string(&path.to_string_lossy().into_owned()).unwrap();
    let module_source = format!(
        "from pathlib import Path\nimport time\nfrom qianxing_strategy import StrategyOutput\n\nSTRATEGY_STATELESS = True\n_MODE = Path({})\n_TRACE = Path({})\n_READY = Path({})\n_RELEASE = Path({})\n\ndef on_event(request):\n    statuses = [update['status'] for update in request.execution_updates]\n    required = {{'submitted', 'accepted', 'partially_filled', 'filled'}}\n    if not required.issubset(set(statuses)):\n        raise RuntimeError(f'execution status facts missing: {{statuses}}')\n    if len(request.fill_updates) != 2:\n        raise RuntimeError(f'expected two committed fills, got {{len(request.fill_updates)}}')\n    status_by_seq = {{update['event_seq']: update['status'] for update in request.execution_updates}}\n    fill_seqs = [update['event_seq'] for update in request.fill_updates]\n    if any(status_by_seq.get(seq) not in {{'partially_filled', 'filled'}} for seq in fill_seqs):\n        raise RuntimeError(f'fill/status sequence pairing missing: {{fill_seqs}} / {{status_by_seq}}')\n    with _TRACE.open('a', encoding='utf-8') as trace:\n        trace.write(str(len(request.execution_updates)) + ':' + ','.join(statuses) + ':' + ','.join(str(seq) for seq in fill_seqs) + '\\n')\n    mode = _MODE.read_text(encoding='utf-8').strip()\n    if mode == 'fail':\n        raise RuntimeError('injected failure after strategy received execution updates')\n    if mode == 'pause':\n        _READY.write_text('ready', encoding='utf-8')\n        deadline = time.monotonic() + 15\n        while not _RELEASE.exists():\n            if time.monotonic() >= deadline:\n                raise RuntimeError('timed out waiting for the lease takeover test')\n            time.sleep(0.01)\n    target = request.positions.get(request.instrument, 0)\n    return StrategyOutput(request_id=request.request_id, strategy_id=request.strategy_id, signal_id=max(1, request.as_of), instrument=request.instrument, target_qty=target, confidence=1000, priority=1, expires_at=request.as_of)\n",
        python_path_literal(&mode_path),
        python_path_literal(&trace_path),
        python_path_literal(&ready_path),
        python_path_literal(&release_path),
    );
    std::fs::write(&module_path, module_source).unwrap();
    config.strategy.python_module = Some(module_name.clone());

    let now = runtime_timestamp_ms();
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let snapshot_path = root.join("bars.json");
    let frame = BarFrame {
        instrument: instrument.clone(),
        source: DataSourceId::new("strategy-execution-updates-worker"),
        ts: vec![now.saturating_sub(1_000)],
        open_raw: vec![100 * SCALE],
        high_raw: vec![101 * SCALE],
        low_raw: vec![99 * SCALE],
        close_raw: vec![100 * SCALE],
        volume_raw: vec![SCALE],
        exact_ohlcv: None,
    };
    std::fs::write(&snapshot_path, frame.to_json()).unwrap();
    config.strategy.bars_snapshot_path = Some(snapshot_path.to_string_lossy().into_owned());
    let runtime_path = root.join("runtime.json");
    std::fs::write(&runtime_path, config.to_json().unwrap()).unwrap();

    let strategy_id = config.strategy.id.as_deref().unwrap();
    let mut order = mk_order(91_701, &instrument, Side::Buy, 2);
    order.trace = Some(OrderTrace {
        strategy_id: Some(strategy_id.into()),
        signal_id: Some(1),
        intent_id: Some(1),
        rule_version: Some("probe-v1".into()),
    });
    let log_name = paper_account_log();
    let mut pipeline = LiveEventPipeline::open(&data_dir, &log_name, "USDT").unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::market_quote(
            instrument.clone(),
            QuoteTick::new(
                now.saturating_sub(6_000),
                Price::from_i64(99),
                Quantity::from_i64(10),
                Price::from_i64(100),
                Quantity::from_i64(10),
                1,
            ),
            now.saturating_sub(6_000),
            1,
            "strategy-probe:quote",
        ))
        .unwrap();
    pipeline
        .register_order(order, now.saturating_sub(5_000))
        .unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: 91_701,
                venue_order_id: Some("venue-91-701".into()),
            },
            now.saturating_sub(4_000),
            now.saturating_sub(4_000),
            2,
            "strategy-probe:accepted",
        ))
        .unwrap();
    for (index, offset) in [3_000_u64, 2_000].into_iter().enumerate() {
        let fill = Fill {
            order_id: 91_701,
            qty: Quantity::from_i64(1),
            price: Price::from_i64(100),
            fee: qx_core::Money::ZERO,
            ts: now.saturating_sub(offset),
            account_id: "main".into(),
            venue_id: Some("paper".into()),
            strategy_id: Some(strategy_id.into()),
            ..Fill::default()
        };
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Fill { fill: fill.clone() },
                fill.ts,
                fill.ts,
                index as u64 + 3,
                format!("strategy-probe:fill-{index}"),
            ))
            .unwrap();
    }
    let through_seq = pipeline.log().events().last().unwrap().seq;
    drop(pipeline);

    let cursor_path = data_dir
        .join("strategy-live-state")
        .join("strategy-paper.execution-cursor.json");
    let first_attempt = run_strategy_worker(&runtime_path, "strategy-paper", true, true);
    assert!(
        first_attempt.is_err(),
        "the injected strategy failure must escape the worker"
    );
    assert!(
        !cursor_path.exists(),
        "a failed strategy callback must leave its EventLog range replayable"
    );
    let queue = FileJobQueue::new(&queue_root);
    let pending = queue.pending().unwrap();
    assert_eq!(pending.len(), 1, "the failed live job must remain queued");
    let queued = &pending[0];
    assert!(queued
        .job
        .input_refs
        .iter()
        .any(|reference| { reference.starts_with("strategy-execution-cursors-v1:") }));
    let run_id = queued.run.run_id;

    // Let another worker take over while the strategy callback is paused. The current worker must
    // revalidate its lease before committing a cursor, then leave the range replayable.
    std::fs::remove_file(queue_root.join("leases").join(format!("{run_id}.json"))).unwrap();
    std::fs::write(&mode_path, "pause").unwrap();
    let worker_runtime_path = runtime_path.clone();
    let worker = std::thread::spawn(move || {
        run_strategy_worker(&worker_runtime_path, "strategy-paper", true, true)
    });
    let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !ready_path.exists() {
        assert!(
            std::time::Instant::now() < ready_deadline,
            "strategy callback did not reach the bounded lease-takeover pause"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let takeover_now = lease_clock(runtime_timestamp_ms()).saturating_add(3_600);
    let takeover = queue
        .claim(run_id, "test-lease-takeover", takeover_now, 60)
        .unwrap();
    assert_eq!(takeover.owner, "test-lease-takeover");
    std::fs::write(&release_path, "continue").unwrap();
    let lost_lease = worker
        .join()
        .expect("strategy worker thread must not panic");
    match lost_lease {
        Err(error) if error.contains("失去租约") && error.contains("test-lease-takeover") => {}
        other => panic!("worker that lost its lease must stop before checkpoint commit: {other:?}"),
    }
    assert!(
        !cursor_path.exists(),
        "a worker that lost its lease must not commit the EventLog cursor"
    );
    assert_eq!(
        queue.pending().unwrap().len(),
        1,
        "lease loss must leave the frozen-range job available to the current owner"
    );
    let lease_path = queue_root.join("leases").join(format!("{run_id}.json"));
    let current_lease: qx_storage::JobLease =
        serde_json::from_slice(&std::fs::read(&lease_path).unwrap()).unwrap();
    assert_eq!(current_lease.owner, takeover.owner);
    assert_eq!(current_lease.fencing_token, takeover.fencing_token);
    std::fs::remove_file(lease_path).unwrap();
    std::fs::write(&mode_path, "ok").unwrap();
    run_strategy_worker(&runtime_path, "strategy-paper", true, true).unwrap();
    let cursor: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&cursor_path).unwrap()).unwrap();
    assert_eq!(cursor["next_seq_by_log"][&log_name], through_seq + 1);
    assert!(
        queue.pending().unwrap().is_empty(),
        "successful retry must ack the job"
    );
    assert!(
        queue_root
            .join("done")
            .join(format!("{run_id}.json"))
            .exists(),
        "successful retry must move the job to the durable done set"
    );
    let calls_after_retry = std::fs::read_to_string(&trace_path).unwrap();
    let call_lines = calls_after_retry.lines().collect::<Vec<_>>();
    assert_eq!(
        call_lines.len(),
        3,
        "callback failure, lost-lease retry, and successful replay must each receive the frozen range"
    );
    assert!(call_lines.iter().all(|line| {
        let fields = line.split(':').collect::<Vec<_>>();
        fields.len() == 3 && fields[0] == "4" && fields[2].contains(',')
    }));

    // A fresh worker process must load the persisted cursor and avoid redelivering old facts.
    run_strategy_worker(&runtime_path, "strategy-paper", true, true).unwrap();
    assert_eq!(
        std::fs::read_to_string(&trace_path)
            .unwrap()
            .lines()
            .count(),
        3,
        "restart after ack must not call the strategy again for an acknowledged range"
    );
    assert!(queue.pending().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn dedicated_paper_spread_recovery_worker_runs_one_scan_without_orders() {
    let root = temp_cli_case_dir("paper-recovery-worker");
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = root.join("data").to_string_lossy().into_owned();
    config
        .workers
        .iter_mut()
        .find(|worker| worker.id == "paper-spread-recovery")
        .expect("paper sample must include disabled spread recovery")
        .enabled = true;
    let runtime = root.join("runtime.json");
    std::fs::write(&runtime, config.to_json().unwrap()).unwrap();
    run_paper_spread_recovery_worker(&runtime, "paper-spread-recovery", true).unwrap();
    assert!(root.join("data").exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn dedicated_spread_recovery_disables_legacy_execution_scan_only_for_same_account_and_venue() {
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    let execution = config
        .workers
        .iter()
        .find(|worker| worker.role == WorkerRole::Execution)
        .cloned()
        .unwrap();
    assert!(!dedicated_spread_recovery_configured(&config, &execution));
    config
        .workers
        .iter_mut()
        .find(|worker| worker.id == "paper-spread-recovery")
        .expect("paper sample must include disabled spread recovery")
        .enabled = true;
    assert!(dedicated_spread_recovery_configured(&config, &execution));
    config.workers.last_mut().unwrap().venue_id = Some("other".into());
    assert!(!dedicated_spread_recovery_configured(&config, &execution));
}

/// #215 行为判据：共享内存传输的启动实参必须带父进程身份，且旗标与值相邻成对。
/// 这条传输没有 stdin 可关，worker 唯一的正常出口是 Rust 侧 `Drop` 把子进程杀掉；父进程
/// 被强杀时 `Drop` 不跑，缺了 `--parent-pid` 就只剩"1 kHz 空转到天荒地老"这一种结局。
#[test]
fn shared_ring_launch_arguments_hand_the_worker_its_parent_identity() {
    let arguments = shared_ring_arguments(
        StrategyTransport::SharedMemoryJson,
        Path::new("ring.input"),
        Path::new("ring.output"),
        SharedRingConfig {
            capacity: 8,
            slot_bytes: 4_096,
        },
        4242,
    );
    for (flag, value) in [
        ("--protocol", "shared_memory_json"),
        ("--input-ring", "ring.input"),
        ("--output-ring", "ring.output"),
        ("--ring-capacity", "8"),
        ("--ring-slot-bytes", "4096"),
        ("--parent-pid", "4242"),
    ] {
        let position = arguments
            .iter()
            .position(|argument| argument == flag)
            .unwrap_or_else(|| panic!("{flag} 没交给 worker: {arguments:?}"));
        assert_eq!(
            arguments[position + 1],
            value,
            "{flag} 与它的值不是相邻成对，worker 会把下一个旗标当成取值"
        );
    }
    // 两条共享传输只差协议名：columnar 若走另一套构造，parent-pid 就能只在其中一条上断。
    let columnar = shared_ring_arguments(
        StrategyTransport::SharedMemoryColumnar,
        Path::new("a.input"),
        Path::new("a.output"),
        SharedRingConfig {
            capacity: 2,
            slot_bytes: 16,
        },
        7,
    );
    assert_eq!(columnar[0], "--protocol");
    assert_eq!(columnar[1], "shared_memory_columnar");
    assert_eq!(
        columnar
            .iter()
            .position(|argument| argument == "--parent-pid")
            .map(|position| columnar[position + 1].as_str()),
        Some("7"),
        "columnar 传输丢了父进程身份"
    );
}

/// #215 跨语言判据：Rust 传出的旗标名与 Python 注册的旗标名必须两侧都还在写，而且 spawn
/// 点交出去的必须是"本进程 pid"。单看任一侧都能自洽，名字漂移或传错 pid 只会表现为
/// "父进程被强杀后 worker 不退出"——那正是本轮要修的故障，不会有别的用例先喊。
/// R17-d 把 C++ 样例 worker 这条第三侧接进同一条用例：名字对得上、值用得上、退出码是 0，
/// 三侧缺任何一侧都在这里红，不用等 CI 的 C++ 冒烟那条独立腿。
#[test]
fn strategy_parent_pid_flag_is_wired_on_both_sides_of_the_language_boundary() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let strategy_worker = std::fs::read_to_string(
        root.join("crates")
            .join("qx-orchestrator")
            .join("src")
            .join("strategy_worker.rs"),
    )
    .unwrap();
    let ring_lifecycle = std::fs::read_to_string(
        root.join("crates")
            .join("qx-orchestrator")
            .join("src")
            .join("strategy_rings.rs"),
    )
    .unwrap();
    assert!(
        ring_lifecycle.contains("Self::create_in(config, &std::env::temp_dir())")
            && ring_lifecycle
                .contains("pub fn create_in(config: SharedRingConfig, directory: &Path)"),
        "生产默认仍用系统临时目录，同时调用方可指定隔离 ring 目录"
    );
    let ring_drop = ring_lifecycle
        .split_once("impl Drop for StrategySharedRings")
        .unwrap_or_else(|| panic!("共享 ring 生命周期没有 Drop 清理出口"))
        .1;
    let unmap_input = ring_drop
        .find("self.input.take()")
        .expect("Drop 必须先释放输入 mmap");
    let unmap_output = ring_drop
        .find("self.output.take()")
        .expect("Drop 必须先释放输出 mmap");
    let remove_input = ring_drop
        .find("remove_file(&self.input_path)")
        .expect("Drop 必须删除输入 ring 文件");
    let remove_output = ring_drop
        .find("remove_file(&self.output_path)")
        .expect("Drop 必须删除输出 ring 文件");
    assert!(
        unmap_input < unmap_output && unmap_output < remove_input && remove_input < remove_output,
        "Windows 需要先解除两份 mmap，再删除 ring 文件：{ring_drop}"
    );
    let spawn_window = strategy_worker
        .split_once("actual_args.extend(shared_ring_arguments(")
        .unwrap_or_else(|| panic!("spawn 点不再经由 shared_ring_arguments 构造共享 ring 实参"))
        .1
        .split_once(");")
        .unwrap()
        .0;
    assert!(
        spawn_window.contains("rings.process_id()"),
        "交给 worker 的不再是本进程 pid：{spawn_window}"
    );
    assert_eq!(
        strategy_worker.matches("\"--parent-pid\".into()").count(),
        1,
        "旗标字面量要么被复制成了两份（漂移时只改一处），要么整个消失"
    );
    let python = std::fs::read_to_string(
        root.join("python")
            .join("qianxing_strategy")
            .join("worker.py"),
    )
    .unwrap();
    assert!(
        python.contains("\"--parent-pid\",") && python.contains("parent_pid=args.parent_pid"),
        "Python worker 不再注册或不再消费 --parent-pid，Rust 传了也没人接"
    );
    assert!(
        python.contains("parent_pid > 0")
            && python.contains("if not _parent_process_alive(parent_pid):"),
        "Python 侧的父进程存活确认不再决定退出，空闲循环会退回无出口空转"
    );
    // 第三侧：C++ 样例 worker 是 Python 之外唯一接得住这份旗标的实现。它自己解析、自己判
    // 存活、自己以 0 退出，三条里断任何一条都退化成同一个故障——父进程被强杀后 ring 常驻，
    // 而这条链路上没有别的用例先喊（V13 R17-d）。
    let cpp = std::fs::read_to_string(root.join("cpp").join("examples").join("jsonl_strategy.cpp"))
        .unwrap();
    assert!(
        cpp.contains("key == \"--parent-pid\"") && cpp.contains("parent_pid = std::stoul(value);"),
        "C++ worker 不再注册或不再消费 --parent-pid，Rust 传了也没人接"
    );
    let cpp_idle_arm = cpp
        .split_once("if (!input.try_pop(encoded)) {")
        .unwrap_or_else(|| {
            panic!("C++ worker 不再有 idle 分支，父进程判定没有了能问这个问题的位置")
        })
        .1
        .split_once("continue;")
        .unwrap()
        .0;
    assert!(
        cpp_idle_arm.contains("if (parent_pid != 0)")
            && cpp_idle_arm.contains("if (!parent_alive(parent_pid)) return 0;"),
        "C++ 的父进程判定不在 idle 臂里、不再把\"没交旗标\"当作不检查，或者不再以 0 退出：{cpp_idle_arm}"
    );
    assert!(
        python.contains("PARENT_LIVENESS_PROBE_SECONDS = 1.0")
            && cpp.contains("kParentLivenessProbeMs = 1000"),
        "两侧探测间隔不再同为 1 秒：Python 改了节流而 C++ 沿用旧值，长空闲下的父进程发现会退化"
    );
}
