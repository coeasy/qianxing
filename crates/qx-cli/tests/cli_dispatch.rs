//! CLI 命令分派契约：`cli.rs` 是唯一分派点，`verify`/`all` 共用同一条自校验链路，
//! 未知命令必须 fail-closed 退出 2 而不是静默落到默认演示。

use std::process::Command;

fn run(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(args)
        .output()
        .expect("启动 qx-cli 失败");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// `verify` 只校验确定性内核；插件装配与 Paper 冒烟属于 `all` 的深度。
#[test]
fn verify_runs_the_deterministic_kernel_only() {
    let (code, stdout, stderr) = run(&["verify"]);
    assert_eq!(code, 0, "verify 失败: {stderr}");
    assert!(
        stdout.contains("同输入两次运行哈希一致 : true")
            && stdout.contains("改参数后哈希发生变化   : true"),
        "verify 必须完成重放双重校验:\n{stdout}"
    );
    assert!(
        !stdout.contains("全部自校验通过"),
        "verify 不应越过内核进入完整自校验:\n{stdout}"
    );
}

/// `all` 在 verify 的基础上继续走完插件装配与 Paper 主链路冒烟。
#[test]
fn all_extends_verify_with_plugin_and_paper_stages() {
    let (code, stdout, stderr) = run(&["all"]);
    assert_eq!(code, 0, "all 失败: {stderr}");
    assert!(
        stdout.contains("卯眼 · 插件装配") && stdout.contains("全部自校验通过 ✓"),
        "all 必须执行插件装配并给出总校验结论:\n{stdout}"
    );
    assert!(
        stdout.contains("针路 · PaperVenue"),
        "all 必须包含 Paper 主链路冒烟:\n{stdout}"
    );
}

/// 未知命令不得静默成功。
#[test]
fn unknown_command_fails_closed() {
    let (code, stdout, stderr) = run(&["definitely-not-a-command"]);
    assert_eq!(code, 2, "未知命令必须退出 2，实际 stdout:\n{stdout}");
    assert!(
        stderr.contains("未知命令"),
        "stderr 需点名未知命令: {stderr}"
    );
}

/// `backtest` 的三个外层位置参数只服务无子命令的统一回测形态。点了子命令又带着它们，
/// clap 会把值绑到外层字段却无人使用——等于静默丢掉一份输入，必须退出 2 并点名是哪几个。
#[test]
fn backtest_rejects_positionals_shadowed_by_a_subcommand() {
    let (code, _, stderr) = run(&[
        "backtest",
        "runtime.json",
        "frame.json",
        "spec.json",
        "builtin",
        "sma_cross",
        "bars.json",
    ]);
    assert_eq!(
        code, 2,
        "被子命令遮蔽的外层参数必须退出 2，实际 stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("runtime/frame/spec") && stderr.contains("不参与回测"),
        "stderr 需点名被丢弃的外层位置参数: {stderr}"
    );
}

/// 反向对照：纯子命令形态与纯位置参数形态都不该触发这条拒绝。
#[test]
fn backtest_still_accepts_either_argument_shape() {
    let (code, _, stderr) = run(&["backtest", "builtin", "sma_cross", "missing-bars.json"]);
    assert_eq!(code, 2, "数据文件缺失应退出 2，实际 stderr:\n{stderr}");
    assert!(
        !stderr.contains("不参与回测"),
        "纯子命令形态不该被拒: {stderr}"
    );
    assert!(
        stderr.contains("内置策略回测失败"),
        "必须走到真实内置入口再失败: {stderr}"
    );
    let (code, _, stderr) = run(&["backtest", "missing-runtime.json", "missing-bars.json"]);
    assert_eq!(code, 2, "数据文件缺失应退出 2，实际 stderr:\n{stderr}");
    assert!(
        !stderr.contains("不参与回测"),
        "无子命令形态不该被拒: {stderr}"
    );
    assert!(
        stderr.contains("统一策略回测失败"),
        "必须走到统一回测入口再失败: {stderr}"
    );
}

/// 上面几颗只覆盖 `verify`/`all`/`backtest`。其余入口此前只有一层薄保障：help 印得出、
/// clap 解得开、`cli.rs` 有分支——三处名字相等并不证明"点之后有人接"。这一批按各自的
/// 口径点名：判据是那条分支自己打出的第一句结论或最后一句失败，而不是 clap 的用法错误。
fn run_in_repo_root(args: &[&str]) -> (i32, String, String) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("仓库根目录")
        .to_path_buf();
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(args)
        .current_dir(&root)
        .output()
        .expect("启动 qx-cli 失败");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// `ecosystem` 是整棵装配树的一键自检：零参数、离线、必须一路打勾退 0。
#[test]
fn ecosystem_smoke_runs_the_whole_assembly() {
    let (code, stdout, stderr) = run_in_repo_root(&["ecosystem"]);
    assert_eq!(code, 0, "ecosystem 失败: {stderr}");
    assert!(
        stdout.contains("[因子 · FactorCatalog]")
            && stdout.contains("[控制 · Query/WebSocket API]"),
        "ecosystem 必须从因子跑到控制面:\n{stdout}"
    );
}

/// `doctor` 与 `runtime-check` 都吃打包好的示例配置，且各自给出自己的结论行。
#[test]
fn doctor_and_runtime_check_answer_on_the_packaged_examples() {
    let (code, stdout, stderr) = run_in_repo_root(&["doctor"]);
    assert_eq!(code, 0, "doctor 失败: {stderr}");
    assert!(
        stdout.contains("[Doctor]"),
        "doctor 需给出体检结论:\n{stdout}"
    );
    let (code, stdout, stderr) = run_in_repo_root(&["runtime-check"]);
    assert_eq!(code, 0, "runtime-check 失败: {stderr}");
    assert!(
        stdout.contains("runtime 引用文件校验通过"),
        "runtime-check 需给出引用校验结论:\n{stdout}"
    );
}

/// `live-check` 对着 production 示例必须 fail-closed 退 2，并点名是前置检查失败。
#[test]
fn live_check_fails_closed_on_the_production_example() {
    let (code, _, stderr) = run_in_repo_root(&["live-check"]);
    assert_eq!(code, 2, "production 示例不带凭据，必须退 2:\n{stderr}");
    assert!(
        stderr.contains("实盘前置检查失败"),
        "stderr 需点名实盘前置检查:\n{stderr}"
    );
    assert!(
        !stderr.contains("未知命令"),
        "live-check 必须是已注册入口: {stderr}"
    );
}

/// `paper-check` 与 `paper-e2e` 是同一颗入口的两个名字，必须给出同一份结论。
///
/// 零参数跑仓库根目录下的打包模板：配置里除 `data_dir` 外还有 `instrument_spec_path`、
/// `jobs_path`、`target_snapshot_path` 三颗相对 `deploy/` 的引用，换任何工作目录都会解析成
/// 另一份（或空）数据，验收便以 `processed=0` 退 2 —— 那测的是隔离手法而不是入口。
/// 两次调用留在同一颗用例里按顺序跑：它们共用 `data/qianxing-paper`（`.gitignore` 的
/// `/data/` 已挡住），并行等于两个进程写同一份运行时状态。
#[test]
fn paper_check_and_paper_e2e_share_one_entry() {
    let (check_code, check_stdout, check_err) = run_in_repo_root(&["paper-check"]);
    let (e2e_code, e2e_stdout, e2e_err) = run_in_repo_root(&["paper-e2e"]);
    assert_eq!(check_code, 0, "paper-check 失败: {check_err}");
    assert_eq!(e2e_code, 0, "paper-e2e 失败: {e2e_err}");
    let last = |text: &str| text.trim().lines().last().unwrap_or_default().to_string();
    assert_eq!(
        last(&check_stdout),
        last(&e2e_stdout),
        "两个名字必须给同一份结论"
    );
    assert!(
        last(&check_stdout).contains("[Paper · E2E]"),
        "结论行缺失:\n{check_stdout}"
    );
}

/// 嵌套命令表：`strategy list` 与 `config explain` 都在 help 里通告过，零参数就该给结论。
#[test]
fn nested_surfaces_answer_by_their_own_names() {
    let (code, stdout, stderr) = run_in_repo_root(&["strategy", "list"]);
    assert_eq!(code, 0, "strategy list 失败: {stderr}");
    assert!(
        stdout.contains("pairs_arbitrage"),
        "策略清单需念出多腿内置策略:\n{stdout}"
    );
    let (code, stdout, stderr) = run_in_repo_root(&["config", "explain"]);
    assert_eq!(code, 0, "config explain 失败: {stderr}");
    assert!(
        stdout.contains("worker id=api") && stdout.contains("未读取密钥内容"),
        "config explain 需给出 worker 概览与安全声明:\n{stdout}"
    );
}

/// 门后入口必须诚实交代自己是被特性挡住的，而不是静默成功或裸 panic。
/// 启用对应特性时只能承诺"走进真实链路后 fail-closed 退 2"，具体文案随链路而变。
#[test]
fn feature_gated_entries_are_honest_about_the_feature() {
    let cases = [
        (
            vec![
                "outbox-relay",
                "missing-root",
                "nats://127.0.0.1:1",
                "qianxing",
            ],
            "outbox-relay 需要使用 --features nats 构建 qx-cli",
        ),
        (
            vec![
                "outbox-relay-postgres",
                "missing-runtime.json",
                "nats://127.0.0.1:1",
                "qianxing",
            ],
            "outbox-relay-postgres 需要使用 --features 'nats postgres' 构建 qx-cli",
        ),
        (
            vec![
                "consumer-dlq-replay",
                "missing-runtime.json",
                "group",
                "event",
            ],
            "consumer-dlq-replay 需要使用 --features nats 构建 qx-cli",
        ),
    ];
    let nats = cfg!(feature = "nats");
    let postgres = cfg!(feature = "postgres");
    for (args, message) in cases {
        let (code, _, stderr) = run_in_repo_root(&args);
        assert_eq!(code, 2, "{args:?} 必须 fail-closed 退 2:\n{stderr}");
        assert!(
            !stderr.contains("未知命令"),
            "{args:?} 必须是已注册入口: {stderr}"
        );
        let gated_off = match args[0] {
            "outbox-relay-postgres" => !(nats && postgres),
            _ => !nats,
        };
        if gated_off {
            assert!(
                stderr.contains(message),
                "{args:?} 未启用特性时必须点名构建特性:\n{stderr}"
            );
        } else {
            assert!(
                !stderr.contains("需要使用 --features"),
                "{args:?} 已启用特性却仍回落成缺特性的说法:\n{stderr}"
            );
        }
    }
}

/// 离线可判定的一批：缺失输入必须走到各自的真实入口再失败，错误里点名是哪条链。
#[test]
fn offline_entries_name_their_own_failure() {
    let cases = [
        (
            vec![
                "ccxt-fetch-ohlcv",
                "missing-ccxt.json",
                "BTCUSDT.BINANCE",
                "1",
                "2",
                "out.json",
            ],
            "CCXT OHLCV 下载失败",
        ),
        (
            vec![
                "ccxt-market-spec",
                "missing-ccxt.json",
                "BTCUSDT.BINANCE",
                "out.json",
            ],
            "CCXT market spec 下载失败",
        ),
        (
            vec![
                "backtest",
                "ccxt-builtin",
                "missing-ccxt.json",
                "sma_cross",
                "BTCUSDT.BINANCE",
                "1",
                "2",
            ],
            "CCXT 内置策略回测失败",
        ),
        (
            vec!["binance-private-probe", "missing-runtime.json"],
            "Binance private probe 失败",
        ),
        (vec!["serve", "missing-runtime.json"], "运行时 API 启动失败"),
        (vec!["supervise", "missing-runtime.json"], "进程监督器停止"),
    ];
    for (args, phrase) in cases {
        let (code, _, stderr) = run_in_repo_root(&args);
        assert_eq!(code, 2, "{args:?} 必须退 2:\n{stderr}");
        assert!(
            stderr.contains(phrase),
            "{args:?} 需走到真实入口再失败，点名 {phrase}:\n{stderr}"
        );
    }
    // 参数校验也在那条分支里、在网络之前：非法 network 词必须被点名，而不是回落到默认值。
    let (code, _, stderr) =
        run_in_repo_root(&["binance-public-probe", "staging", "BTCUSDT.BINANCE"]);
    assert_eq!(code, 2, "未知 network 必须退 2:\n{stderr}");
    assert!(
        stderr.contains("network 必须是 testnet 或 mainnet"),
        "stderr 需点名 network 词表:\n{stderr}"
    );
}
