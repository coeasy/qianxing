//!
//! 缺陷长在命令行入口的默认值上：这里曾无条件取仓库那份 BTCUSDT 示例夹具，于是
//! `init --profile ashare` 生成的项目照 README 敲 `backtest qianxing.runtime.json`，
//! 读的是另一个标的的行情。只有真子进程能同时覆盖"读配置"和"挑默认路径"两条腿，
//! 因此三条用例都走 `CARGO_BIN_EXE_qx-cli`。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn deploy(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("仓库根目录")
        .join("deploy")
        .join(name)
}

fn temp_dir(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "qianxing-backtest-frame-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("创建用例临时目录失败");
    root
}

/// 在 `cwd` 里执行一条命令；产物落点跟 `storage.data_dir` 走，用例目录一律隔离在临时树。
fn run(cwd: &Path, args: &[String]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("启动 qx-cli 失败");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// 取 stdout 里某个键的值：`dataset=` 在 `[Data · Dataset]` 行，`result_hash=` 在 `[RunManifest]` 行。
fn value_of(stdout: &str, key: &str) -> String {
    stdout
        .split_whitespace()
        .find(|token| token.starts_with(&format!("{key}=")))
        .map(|token| token.trim_start_matches(&format!("{key}=")).to_string())
        .unwrap_or_default()
}

/// 按 `edit` 改动 A 股项目运行时，另存一份到同一目录。
fn rewritten_runtime(root: &Path, edit: impl FnOnce(&mut serde_json::Value)) -> PathBuf {
    let path = root.join("qianxing.runtime.case.json");
    let payload = std::fs::read_to_string(root.join("qianxing.runtime.json"))
        .expect("读取 init 生成的运行时失败");
    let mut value: serde_json::Value = serde_json::from_str(&payload).unwrap();
    edit(&mut value);
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).expect("写入用例运行时失败");
    path
}

fn init_ashare_project(root: &Path) -> PathBuf {
    let runtime = root.join("qianxing.runtime.json");
    let (code, stdout, stderr) = run(
        root,
        &[
            "init".into(),
            runtime.to_string_lossy().into_owned(),
            "--profile".into(),
            "ashare".into(),
        ],
    );
    assert_eq!(code, 0, "init --profile ashare 必须成功: {stdout} {stderr}");
    runtime
}

/// A 股项目只喂配置：必须读自己声明的那份 bars，并与显式指同一路径的形态同结果。
#[test]
fn config_only_backtest_reads_the_declared_ashare_bars() {
    let root = temp_dir("ashare");
    let runtime = init_ashare_project(&root);
    let runtime_text = runtime.to_string_lossy().into_owned();
    let declared = root.join("qianxing.ashare.bar-frame.example.json");
    let declared_text = declared.to_string_lossy().into_owned();
    assert!(declared.is_file(), "init 应把 A 股 BarFrame 复制进项目");

    let (only_code, only_stdout, only_stderr) =
        run(&root, &["backtest".into(), runtime_text.clone()]);
    assert_eq!(only_code, 0, "只喂配置的 A 股回测必须跑通: {only_stderr}");
    assert_eq!(
        value_of(&only_stdout, "dataset"),
        "strategy-bars:000001.SZSE",
        "默认 BarFrame 必须是配置声明的 A 股夹具: {only_stdout}"
    );

    let (explicit_code, explicit_stdout, explicit_stderr) = run(
        &root,
        &[
            "backtest".into(),
            runtime_text.clone(),
            declared_text.clone(),
        ],
    );
    assert_eq!(
        explicit_code, 0,
        "显式指定同一份 BarFrame 必须跑通: {explicit_stderr}"
    );
    assert_eq!(
        value_of(&only_stdout, "result_hash"),
        value_of(&explicit_stdout, "result_hash"),
        "默认值只是省略参数，不能换成另一份行情"
    );
    assert_ne!(
        value_of(&only_stdout, "result_hash"),
        "",
        "结果摘要要有 result_hash"
    );

    let (strategy_code, strategy_stdout, strategy_stderr) = run(
        &root,
        &[
            "strategy".into(),
            "backtest".into(),
            runtime_text,
            declared_text,
        ],
    );
    assert_eq!(strategy_code, 0, "策略链同形态必须跑通: {strategy_stderr}");
    assert_eq!(
        value_of(&strategy_stdout, "result_hash"),
        value_of(&only_stdout, "result_hash"),
        "统一入口与策略链必须落在同一份行情上"
    );
}

/// 声明了的文件缺席时，不能悄悄换成示例夹具跑出一份没声明过的结果：要按声明的路径报错。
#[test]
fn missing_declared_bars_fails_closed_instead_of_swapping_dataset() {
    let root = temp_dir("missing");
    init_ashare_project(&root);
    let missing = root.join("no-such-bars.json");
    let runtime = rewritten_runtime(&root, |value| {
        value["strategy"]["bars_snapshot_path"] =
            serde_json::Value::String(missing.to_string_lossy().into_owned());
    });
    let (code, stdout, stderr) = run(
        &root,
        &["backtest".into(), runtime.to_string_lossy().into_owned()],
    );
    assert_eq!(code, 2, "声明的 bars 缺席必须止步: {stdout}");
    assert!(
        stderr.contains(&missing.to_string_lossy().into_owned())
            || stdout.contains(&missing.to_string_lossy().into_owned()),
        "报错要点名配置声明的那份文件: {stderr}"
    );
    assert!(
        !stderr.contains("fingerprint 不匹配"),
        "不能再拿另一份夹具的指纹差当报错: {stderr}"
    );
}

/// 仓库自带的 BTC 示例配置仍要跑得动：默认回落那一条腿没有被改断。
#[test]
fn btc_example_runtime_still_backtests_on_its_declared_frame() {
    let root = temp_dir("btc");
    let runtime = deploy("qianxing.runtime.builtin-strategy.example.json");
    let (code, stdout, stderr) = run(
        &root,
        &["backtest".into(), runtime.to_string_lossy().into_owned()],
    );
    assert_eq!(code, 0, "BTC 示例只喂配置必须跑通: {stderr}");
    assert_eq!(
        value_of(&stdout, "dataset"),
        "strategy-bars:BTCUSDT.BINANCE",
        "{stdout}"
    );
    assert_ne!(value_of(&stdout, "result_hash"), "", "{stdout}");
}

/// 顶层没有声明时不猜各策略各自的 bars：仍按既有回落口径取示例夹具（无参演示那一条腿）。
#[test]
fn undeclared_top_level_bars_does_not_guess_a_strategy_bars() {
    let root = temp_dir("undeclared");
    // ccxt 示例：顶层 strategy 不声明 bars，两条策略 worker 各指一份实时快照（本地并不存在）。
    let runtime = deploy("qianxing.runtime.ccxt.example.json");
    let (code, stdout, stderr) = run(
        &root,
        &["backtest".into(), runtime.to_string_lossy().into_owned()],
    );
    assert_eq!(
        value_of(&stdout, "dataset"),
        "strategy-bars:BTCUSDT.BINANCE",
        "顶层缺席时回落示例夹具: {stdout} {stderr}"
    );
    assert_ne!(
        code, 0,
        "该配置缺 market spec，回落后仍要由下游如实止步: {stdout}"
    );
}
