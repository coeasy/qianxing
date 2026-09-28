//! `strategy.builtin_*` 信号参数在四条回测链上的读取（V11 Q64）。
//!
//! 缺陷形状与 Q61 同族：`backtest builtin` 收下 `--config` 却只吃 `BuiltinStrategyConfig::new`
//! 的 5/20/14/100 写死默认，于是同一份配置在 `strategy backtest` 与 `backtest builtin` 上跑的是
//! 两套信号；深度档与多腿链同样读得到配置，同样把参数丢掉。用例走真实子进程，断言
//! "参数换了结果真的换"，而不是只断言参数被印出来。
//!
//! 除了"读得到"，还钉住读到的顺序：非法组合必须在那行 `[X · Signal]` 印出来之前就被拒 ——
//! 先宣告口径再报错，等于往 stdout 留下一套从没跑过的参数。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const FRAME: &str = "qianxing.ashare.bar-frame.example.json";
const SPEC: &str = "qianxing.ashare.spot.spec.json";
const RUNTIME: &str = "qianxing.runtime.ashare.example.json";

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
        "qianxing-builtin-signal-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("创建用例临时目录失败");
    root
}

fn run<A: AsRef<OsStr>>(args: &[A]) -> (i32, String, String) {
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

fn line_with(stdout: &str, marker: &str) -> String {
    stdout
        .lines()
        .find(|line| line.starts_with(marker))
        .unwrap_or("")
        .to_string()
}

fn field(stdout: &str, marker: &str, key: &str) -> String {
    stdout
        .lines()
        .find(|line| line.starts_with(marker))
        .and_then(|line| line.split(&format!("{key}=")).nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_default()
        .to_string()
}

/// 一个用例里会造多份配置，文件必须各自独立：同名会把后写的档位盖掉前一档，
/// 于是"阈值 0 变松"实际跑的是阈值 500 —— 假对照。
static CASE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 用示例配置作底，但把 A 股段摘掉：本轮只让信号参数这一条腿动，
/// 免得整手拒单（Q61）替参数背结果。路径一律绝对化，数据集目录改到用例临时目录。
fn signal_config(root: &Path, edit: impl FnOnce(&mut serde_json::Value)) -> PathBuf {
    let template = std::fs::read_to_string(deploy(RUNTIME)).expect("读取示例运行时配置失败");
    let mut value: serde_json::Value = serde_json::from_str(&template).unwrap();
    let bars = {
        let strategy = value["strategy"].as_object_mut().unwrap();
        for key in [
            "ashare_rules_path",
            "ashare_actions_path",
            "ashare_calendar_path",
            "dataset_bundle_path",
        ] {
            strategy.remove(key);
        }
        strategy
            .get("bars_snapshot_path")
            .and_then(|v| v.as_str())
            .map(|text| deploy(text).to_string_lossy().into_owned())
    };
    value["storage"]["data_dir"] =
        serde_json::Value::String(root.join("data").to_string_lossy().into_owned());
    if let Some(bars) = bars {
        value["strategy"]["bars_snapshot_path"] = serde_json::Value::String(bars);
    }
    edit(&mut value);
    let seq = CASE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = root.join(format!("runtime.signal.case-{seq}.json"));
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).expect("写入用例配置失败");
    path
}

fn set(strategy: &mut serde_json::Value, key: &str, value: i64) {
    strategy[key] = serde_json::Value::from(value);
}

fn builtin(kind: &str, quantity: &str, config: Option<&Path>) -> (i32, String, String) {
    let mut args: Vec<String> = vec![
        "backtest".into(),
        "builtin".into(),
        kind.into(),
        deploy(FRAME).to_string_lossy().into_owned(),
        deploy(SPEC).to_string_lossy().into_owned(),
        quantity.into(),
    ];
    if let Some(config) = config {
        args.extend(["--config".into(), config.to_string_lossy().into_owned()]);
    }
    run(&args)
}

/// 窗口对：默认 5/20 在这 6 根 Bar 上永远不交叉，配置里的 2/3 会成交。
#[test]
fn declared_windows_change_the_builtin_result() {
    let root = temp_dir("windows");
    let config = signal_config(&root, |_| {});
    let (_, plain, plain_err) = builtin("ema_cross", "100", None);
    assert!(
        line_with(&plain, "[Builtin · Signal]").contains("source=builtin-default"),
        "不读配置时必须报出用的是默认口径: {plain_err}"
    );
    assert_eq!(field(&plain, "[Builtin · Backtest]", "fills"), "0");
    let (code, bound, bound_err) = builtin("ema_cross", "100", Some(&config));
    assert_eq!(code, 0, "配置里的信号参数必须被接受: {bound_err}");
    assert_eq!(
        field(&bound, "[Builtin · Backtest]", "fills"),
        "1",
        "窗口换了却没换成交，说明参数没进策略"
    );
    assert!(
        line_with(&bound, "[Builtin · Signal]")
            .contains("source=config fast_window=2 slow_window=3"),
        "用了哪套信号口径要印出来: {}",
        line_with(&bound, "[Builtin · Signal]")
    );
    assert_ne!(
        field(&plain, "[Builtin · Backtest]", "result_hash"),
        field(&bound, "[Builtin · Backtest]", "result_hash")
    );
}

/// 周期与阈值各自都是活参数：换值就换结果，不是印出来好看。
#[test]
fn declared_period_and_threshold_each_move_the_result() {
    let root = temp_dir("period-threshold");
    let short_rsi = signal_config(&root, |value| {
        set(&mut value["strategy"], "builtin_period", 2)
    });
    let (_, rsi_default, err) = builtin("rsi", "100", None);
    assert_eq!(
        field(&rsi_default, "[Builtin · Backtest]", "fills"),
        "0",
        "默认周期 14 在这 6 根 Bar 上算不出指标: {err}"
    );
    let (code, rsi_short, err) = builtin("rsi", "100", Some(&short_rsi));
    assert_eq!(code, 0, "合法周期必须跑通: {err}");
    assert_eq!(field(&rsi_short, "[Builtin · Backtest]", "fills"), "1");
    assert!(line_with(&rsi_short, "[Builtin · Signal]").contains("period=2"));

    let loose_grid = signal_config(&root, |value| {
        set(&mut value["strategy"], "builtin_threshold_bps", 0)
    });
    let tight_grid = signal_config(&root, |value| {
        set(&mut value["strategy"], "builtin_threshold_bps", 500)
    });
    let (_, grid_default, grid_default_err) = builtin("grid", "100", None);
    assert_eq!(
        field(&grid_default, "[Builtin · Backtest]", "fills"),
        "2",
        "默认阈值下 grid 的基线成交数要固定，否则下面的三值对比无意义: {grid_default_err}"
    );
    let (code, grid_loose, err) = builtin("grid", "100", Some(&loose_grid));
    assert_eq!(code, 0, "阈值 0 必须跑通: {err}");
    let (code, grid_tight, err) = builtin("grid", "100", Some(&tight_grid));
    assert_eq!(code, 0, "阈值 500 必须跑通: {err}");
    let hashes = [
        field(&grid_default, "[Builtin · Backtest]", "result_hash"),
        field(&grid_loose, "[Builtin · Backtest]", "result_hash"),
        field(&grid_tight, "[Builtin · Backtest]", "result_hash"),
    ];
    assert_eq!(field(&grid_loose, "[Builtin · Backtest]", "fills"), "1");
    assert_eq!(field(&grid_tight, "[Builtin · Backtest]", "fills"), "0");
    assert_eq!(
        hashes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3,
        "三个阈值档位必须给出三个不同结果，否则参数仍是死配置"
    );
}

/// 配置里写了非法参数不能退成"按默认跑"：整轮失败。
#[test]
fn illegal_declared_parameters_fail_closed() {
    let root = temp_dir("illegal");
    let config = signal_config(&root, |value| {
        let strategy = &mut value["strategy"];
        set(strategy, "builtin_fast_window", 9);
        set(strategy, "builtin_slow_window", 3);
    });
    let (code, _, stderr) = builtin("ema_cross", "100", Some(&config));
    assert_eq!(code, 2, "非法窗口组合必须拒绝而不是静默兜底: {stderr}");
    assert!(
        stderr.contains("builtin_fast_window") && stderr.contains("builtin_slow_window"),
        "报错要点名是哪两个键冲突: {stderr}"
    );
}

/// 只给单边窗口是另一类非法组合：运行时体检看不到（它只比两个都写了的键），要等这项并进
/// 该链自己的默认窗口之后才成立。所以复检必须发生在覆盖之后、印生效口径之前 —— 顺序错了，
/// stdout 上就会留下一套从没跑过的参数。
#[test]
fn single_sided_override_is_rechecked_before_the_provenance_line() {
    let root = temp_dir("single-sided");
    let wide_fast = signal_config(&root, |value| {
        value["strategy"]
            .as_object_mut()
            .unwrap()
            .remove("builtin_slow_window");
        set(&mut value["strategy"], "builtin_fast_window", 25);
    });
    let (code, stdout, stderr) = builtin("ema_cross", "100", Some(&wide_fast));
    assert_eq!(code, 2, "25 并进默认慢窗 20 之后必须被拒: {stderr}");
    assert!(
        stderr.contains("fast_window=25") && stderr.contains("slow_window=20"),
        "报错要给覆盖后的生效值，只说\"参数非法\"没法定位是哪个键: {stderr}"
    );
    assert!(
        !stdout.contains("[Builtin · Signal]"),
        "口径行只能在参数体检通过后才准印，否则等于宣告了一套没跑过的参数:\n{stdout}"
    );
    let (code, stdout, stderr) = run(&[
        "backtest".into(),
        "book".into(),
        "--fill-tier".into(),
        "l1".into(),
        "--root".into(),
        root.join("book").to_string_lossy().into_owned(),
        // 体检按 kind 的清单做（V12 #102）：这条链上一轮用的是 grid，它只读阈值，
        // 25/20 那对窗口对它既不改结果、也不该把整轮拒掉。这里要验的是"复检先于印口径"
        // 这个顺序，所以点一个真的读快慢窗口的 kind。
        "ema_cross".into(),
        deploy("qianxing.depth-frame.l1.example.json")
            .to_string_lossy()
            .into_owned(),
        "--config".into(),
        wide_fast.to_string_lossy().into_owned(),
    ]);
    assert_eq!(code, 2, "深度档同样要在复检之后才印口径: {stderr}");
    assert!(
        !stdout.contains("[Depth · Signal]"),
        "深度档的口径行不能先于体检落地:\n{stdout}"
    );
    let (code, stdout, stderr) = run(&[
        "strategy".into(),
        "backtest".into(),
        wide_fast.to_string_lossy().into_owned(),
        deploy(FRAME).to_string_lossy().into_owned(),
        deploy(SPEC).to_string_lossy().into_owned(),
    ]);
    assert_eq!(code, 2, "策略链也要在覆盖后复检: {stderr}");
    assert!(
        !stdout.contains("[Strategy · Signal]"),
        "策略链的口径行不能先于体检落地:\n{stdout}"
    );
}

/// 两条 Bar 链读同一份配置就该跑同一套信号：成交笔数、收益与回撤逐项相等。
/// 配置取用例自己那份副本（`data_dir` 已改到临时目录）：直接拿仓库里的示例跑，产物就会
/// 落进 `deploy/data`，下一次改动摘要格式时"同一产物路径内容必须一致"的闸门会先把
/// 仓库里那份旧产物当成对手，而不是让用例变红（V12 #82）。
#[test]
fn both_bar_chains_read_the_same_declared_signal() {
    let root = temp_dir("same-signal");
    let config = signal_config(&root, |_| {});
    let (code, strategy_chain, err) = run(&[
        "strategy".into(),
        "backtest".into(),
        config.to_string_lossy().into_owned(),
        deploy(FRAME).to_string_lossy().into_owned(),
        deploy(SPEC).to_string_lossy().into_owned(),
    ]);
    assert_eq!(code, 0, "策略链基线必须跑通: {err}");
    let (code, builtin_chain, err) = builtin("ema_cross", "100", Some(&config));
    assert_eq!(code, 0, "内置链同一份配置必须跑通: {err}");
    let marker = "[Builtin · Backtest]".to_string();
    // 两条链都得真的成交：零成交下"逐项相等"可以永远成立，那就不是在核对同一套信号。
    assert_ne!(
        field(&strategy_chain, "[Strategy · Backtest]", "fills"),
        "0",
        "策略链基线零成交，下面的对比是空的: {strategy_chain}"
    );
    for key in ["fills", "return_bps", "max_drawdown_bps"] {
        assert_eq!(
            field(&strategy_chain, "[Strategy · Backtest]", key),
            field(&builtin_chain, &marker, key),
            "同一份配置在两条 Bar 链上的 {key} 不同，说明信号口径又分叉了"
        );
    }
    assert!(line_with(&builtin_chain, "[Builtin · Signal]").contains("source=config"));
    assert!(
        line_with(&strategy_chain, "[Strategy · Signal]").contains("source=config"),
        "策略链也要印出生效口径，否则读者无从判断配置是否被认账: {}",
        line_with(&strategy_chain, "[Strategy · Signal]")
    );
    let params = |line: &str| {
        line.split_once("Signal] ")
            .map(|(_, rest)| rest.split(" quantity=").next().unwrap_or("").to_string())
            .unwrap_or_default()
    };
    assert_eq!(
        params(&line_with(&builtin_chain, "[Builtin · Signal]")),
        params(&line_with(&strategy_chain, "[Strategy · Signal]")),
        "两条链印出来的来源判词与四项参数必须逐项相同"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 深度档链也收 `--config`（风控与成本都从它取），信号参数不能只在这条链上失效（V11 Q64）。
#[test]
fn depth_chain_applies_the_declared_signal() {
    let root = temp_dir("depth");
    let default_grid = signal_config(&root, |value| {
        let strategy = &mut value["strategy"];
        strategy["builtin_strategy"] = serde_json::Value::String("grid".into());
        set(strategy, "builtin_threshold_bps", 100);
    });
    let loose_grid = signal_config(&root, |value| {
        let strategy = &mut value["strategy"];
        strategy["builtin_strategy"] = serde_json::Value::String("grid".into());
        set(strategy, "builtin_threshold_bps", 0);
    });
    let frame = deploy("qianxing.depth-frame.l1.example.json")
        .to_string_lossy()
        .into_owned();
    let book = |config: &Path| {
        run(&[
            "backtest".into(),
            "book".into(),
            "--fill-tier".into(),
            "l1".into(),
            "--root".into(),
            root.join("book").to_string_lossy().into_owned(),
            "grid".into(),
            frame.clone(),
            "--config".into(),
            config.to_string_lossy().into_owned(),
        ])
    };
    let (code, tight, err) = book(&default_grid);
    assert_eq!(code, 0, "深度档默认阈值基线必须跑通: {err}");
    let (code, loose, err) = book(&loose_grid);
    assert_eq!(code, 0, "深度档换阈值后必须跑通: {err}");
    assert_eq!(field(&tight, "[Depth · Backtest]", "fills"), "0");
    assert_eq!(
        field(&loose, "[Depth · Backtest]", "fills"),
        "1",
        "阈值放宽却不换成交，说明深度链仍用写死默认"
    );
    assert_ne!(
        field(&tight, "[Depth · Backtest]", "result_hash"),
        field(&loose, "[Depth · Backtest]", "result_hash")
    );
    assert!(
        line_with(&loose, "[Depth · Signal]").contains("source=config"),
        "深度链要印出生效的信号口径: {}",
        line_with(&loose, "[Depth · Signal]")
    );
}

/// 多腿链同样收 `--config`：一条腿上写死窗口，两条腿的信号就与配置无关了。
#[test]
fn multi_leg_chain_applies_the_declared_signal() {
    let root = temp_dir("multi");
    let pair_config = |threshold: i64| {
        signal_config(&root, |value| {
            let strategy = &mut value["strategy"];
            strategy["builtin_strategy"] = serde_json::Value::String("pairs_arbitrage".into());
            strategy["builtin_reference_instrument"] =
                serde_json::Value::String("ETHUSDT.BINANCE".into());
            for (key, fixture) in [
                (
                    "bars_snapshot_path",
                    "qianxing.bar-frame.pairs-primary.example.json",
                ),
                (
                    "builtin_reference_bars_snapshot_path",
                    "qianxing.bar-frame.pairs-reference.example.json",
                ),
            ] {
                strategy[key] =
                    serde_json::Value::String(deploy(fixture).to_string_lossy().into_owned());
            }
            set(strategy, "builtin_threshold_bps", threshold);
        })
    };
    let close_grid = pair_config(0);
    let far_grid = pair_config(10000);
    let multi = |config: &Path| {
        run(&[
            "backtest".into(),
            "multi-builtin".into(),
            "pairs_arbitrage".into(),
            deploy("qianxing.bar-frame.pairs-primary.example.json")
                .to_string_lossy()
                .into_owned(),
            deploy("qianxing.bar-frame.pairs-reference.example.json")
                .to_string_lossy()
                .into_owned(),
            "--root".into(),
            root.join("multi").to_string_lossy().into_owned(),
            "--config".into(),
            config.to_string_lossy().into_owned(),
        ])
    };
    let (code, near, err) = multi(&close_grid);
    assert_eq!(code, 0, "阈值放宽的多腿回测必须跑通: {err}");
    let (code, none, err) = multi(&far_grid);
    assert_eq!(code, 0, "阈值收紧的多腿回测必须跑通: {err}");
    assert_ne!(
        field(&near, "[Multi-leg · Backtest]", "combined_return_bps"),
        "0",
        "基线本身要真的成交，否则下面的对比是空的: {near}"
    );
    assert_eq!(
        field(&none, "[Multi-leg · Backtest]", "fills"),
        "0",
        "阈值收到 10000bp 就不该还有成交"
    );
    assert_eq!(
        field(&none, "[Multi-leg · Backtest]", "combined_return_bps"),
        "0",
        "参数换了结果没换，说明多腿链仍用写死默认"
    );
    assert!(
        line_with(&none, "[Multi · Signal]").contains("threshold_bps=10000"),
        "多腿链要把生效阈值印进 stdout: {}",
        line_with(&none, "[Multi · Signal]")
    );
}
