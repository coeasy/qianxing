//! 策略与 Paper worker 入口的行为用例：从运行时配置读到一次真实调用，不经过回测链。
//!
//! 这些用例原先住在 `backtest_entries.rs`：它们验的是 worker 侧的入口，与 Bar 回测
//! 装配无关，Phase 4o 的主题目录按入口所在分文件。

use super::*;

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
    let output =
        invoke_builtin_strategy(&root, &config, &instrument, "builtin-worker-test", u64::MAX)
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
        crate::strategy_host::builtin_strategy_config_from_runtime(&declared.strategy, &instrument)
            .unwrap();
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
        crate::strategy_host::builtin_strategy_config_from_runtime(&cleared.strategy, &instrument)
            .unwrap();
    assert_eq!(
        windows(&fallback),
        (5, 20, 14, 100),
        "四项全缺时才该回到写死默认"
    );

    // 只给单边窗口时，运行时体检看不到（它只比两个都写了的键）；非法组合要等这项并进
    // 默认慢窗之后才成立，所以装配尾部必须复检。
    let mut one_sided = declared.clone();
    one_sided.strategy.builtin_slow_window = None;
    one_sided.strategy.builtin_fast_window = Some(25);
    let error = crate::strategy_host::builtin_strategy_config_from_runtime(
        &one_sided.strategy,
        &instrument,
    )
    .unwrap_err();
    assert!(
        error.contains("内置策略参数非法"),
        "单边窗口的非法组合没有被复检: {error}"
    );
}

#[test]
fn dedicated_paper_spread_recovery_worker_runs_one_scan_without_orders() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-recovery-worker-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
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
