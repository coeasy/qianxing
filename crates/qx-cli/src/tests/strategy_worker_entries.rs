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
    // 默认慢窗之后才成立，所以装配尾部必须复检。体检按 kind 的清单做（V12 #102）：
    // 例子里的 macd 一个窗口都不读，25/20 那种"看着非法"的组合对它既不改结果、也不该拒。
    let mut one_sided = declared.clone();
    one_sided.strategy.builtin_strategy = Some("ema_cross".into());
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
    let mut unused_illigal = declared.clone();
    unused_illigal.strategy.builtin_fast_window = Some(9);
    unused_illigal.strategy.builtin_slow_window = Some(3);
    let ignored = crate::strategy_host::builtin_strategy_config_from_runtime(
        &unused_illigal.strategy,
        &instrument,
    )
    .unwrap_or_else(|error| panic!("macd 不读快慢窗口，9/3 这种清单外取值不许拒一轮交易: {error}"));
    assert_eq!(
        windows(&ignored),
        (9, 3, 9, 50),
        "清单外的取值仍要原样装配（播报才写得出 declared_unused），只是不参与信号"
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
    let rust = std::fs::read_to_string(
        root.join("crates")
            .join("qx-cli")
            .join("src")
            .join("strategy_host.rs"),
    )
    .unwrap();
    let spawn_window = rust
        .split_once("actual_args.extend(shared_ring_arguments(")
        .unwrap_or_else(|| panic!("spawn 点不再经由 shared_ring_arguments 构造共享 ring 实参"))
        .1
        .split_once(");")
        .unwrap()
        .0;
    assert!(
        spawn_window.contains("std::process::id()"),
        "交给 worker 的不再是本进程 pid：{spawn_window}"
    );
    assert_eq!(
        rust.matches("\"--parent-pid\".into()").count(),
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
