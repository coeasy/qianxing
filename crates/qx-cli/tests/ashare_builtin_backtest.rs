//! `backtest builtin` 一族入口对 `--config` 里 A 股段的处理（V11 Q61）。
//!
//! 走真实子进程：本缺陷正好长在命令行入口上，同进程调用函数复制不到那条读配置的腿。

use std::ffi::OsStr;
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
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("仓库根目录");
    let root = workspace.join("target").join(format!(
        "qianxing-ashare-builtin-{label}-{}-{}",
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

fn run_from<A: AsRef<OsStr>>(working_directory: &Path, args: &[A]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(working_directory)
        .args(args)
        .output()
        .expect("启动 qx-cli 失败");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn summary(stdout: &str) -> serde_json::Value {
    let path = stdout
        .lines()
        .find_map(|line| {
            line.strip_prefix("[Artifacts] summary=")
                .and_then(|value| value.split_once(" equity=").map(|(path, _)| path))
        })
        .expect("builtin must expose its durable summary path");
    serde_json::from_slice(&std::fs::read(path).expect("read builtin summary"))
        .expect("builtin summary is JSON")
}

const FRAME: &str = "qianxing.ashare.bar-frame.example.json";
const SPEC: &str = "qianxing.ashare.spot.spec.json";
const RUNTIME: &str = "qianxing.runtime.ashare.example.json";

/// 把示例运行时配置里的路径改成绝对路径，写进临时目录，并按 `edit` 改动 JSON。
fn config_with(root: &Path, edit: impl FnOnce(&mut serde_json::Value)) -> PathBuf {
    std::fs::create_dir_all(root).expect("创建用例配置目录失败");
    let template = std::fs::read_to_string(deploy(RUNTIME)).expect("读取示例运行时配置失败");
    let mut value: serde_json::Value = serde_json::from_str(&template).unwrap();
    absolutize_paths(&mut value, root);
    // Q64 之后 `--config` 的 `builtin_*` 窗口/周期/阈值会真的换掉信号，示例里那套 2/3 与本用例
    // 无配置基线（内置默认 5/20/14/100）不同。本文件只证明 A 股段这一条腿，所以把四项摘掉，
    // 让"配置 vs 不配置"的差值只剩 A 股规则与费率。
    for key in [
        "builtin_fast_window",
        "builtin_slow_window",
        "builtin_period",
        "builtin_threshold_bps",
    ] {
        value["strategy"].as_object_mut().unwrap().remove(key);
    }
    edit(&mut value);
    let path = root.join("runtime.ashare.case.json");
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).expect("写入用例配置失败");
    path
}

fn config_without_ashare(root: &Path) -> PathBuf {
    config_with(root, |value| {
        value["strategy"]
            .as_object_mut()
            .unwrap()
            .retain(|key, _| !key.starts_with("ashare_"));
        value["storage"]["data_dir"] =
            serde_json::Value::String(root.join("plain-data").to_string_lossy().into_owned());
    })
}

/// 把配置里指向 deploy 资产的路径改成绝对路径（数据集目录改到用例临时目录）。
///
/// `strategy backtest` 在走到 A 股绑定之前还要读数据集与产物目录，这些键不绝对化的话
/// 用例会被无关的"文件找不到"挡住，证明不了配对规则。
fn absolutize_paths(value: &mut serde_json::Value, root: &Path) {
    value["storage"]["data_dir"] =
        serde_json::Value::String(root.join("data").to_string_lossy().into_owned());
    for key in [
        "ashare_rules_path",
        "ashare_actions_path",
        "ashare_calendar_path",
        "dataset_bundle_path",
        "bars_snapshot_path",
        "target_snapshot_path",
        "research_snapshot_path",
    ] {
        if let Some(text) = value["strategy"].get(key).and_then(|v| v.as_str()) {
            let absolute = deploy(text).to_string_lossy().into_owned();
            value["strategy"][key] = serde_json::Value::String(absolute);
        }
    }
}

fn builtin_args(config: Option<&Path>, quantity: &str) -> Vec<String> {
    builtin_args_with_spec(config, quantity, &deploy(SPEC))
}

fn builtin_args_with_spec(config: Option<&Path>, quantity: &str, spec: &Path) -> Vec<String> {
    let mut args = vec![
        "backtest".into(),
        "builtin".into(),
        "grid".into(),
        deploy(FRAME).to_string_lossy().into_owned(),
        spec.to_string_lossy().into_owned(),
        quantity.into(),
    ];
    if let Some(config) = config {
        args.extend(["--config".into(), config.to_string_lossy().into_owned()]);
    }
    args
}

/// 整手规则改变结果：不读配置时 150 股照常成交，读了配置必须按 A 股口径拒单。
#[test]
fn builtin_entry_binds_the_declared_ashare_rules() {
    let root = temp_dir("bind");
    let config = config_with(&root.join("bound"), |_| {});
    let plain_config = config_without_ashare(&root.join("plain"));
    // 这条用例只比较 A 股交易规则快照是否接入。为使 150 股成为合法的基线订单，
    // 使用一股步长的市场规格；绑定配置后的整手拒单来自 A 股规则快照。
    let generic_spec = root.join("generic-qty-step.spec.json");
    let mut spec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(deploy(SPEC)).unwrap()).unwrap();
    spec["qty_step_raw"] = serde_json::Value::from(1_000_000_000_i64);
    spec["min_qty_raw"] = serde_json::Value::from(1_000_000_000_i64);
    std::fs::write(&generic_spec, serde_json::to_vec(&spec).unwrap()).unwrap();

    let (plain_code, plain, plain_err) = run(&builtin_args_with_spec(
        Some(&plain_config),
        "150",
        &generic_spec,
    ));
    assert_eq!(plain_code, 0, "不绑定 A 股规则的基线必须跑通: {plain_err}");
    let plain_summary = summary(&plain);
    assert_eq!(plain_summary["fills"], 2);
    assert_eq!(plain_summary["rejected_orders"], 0);
    assert!(!plain_summary["model_descriptors"]
        .as_array()
        .is_some_and(|items| items
            .iter()
            .any(|item| item.as_str().is_some_and(|v| v.starts_with("AshareRules@")))));

    let (code, bound, bound_err) =
        run(&builtin_args_with_spec(Some(&config), "150", &generic_spec));
    assert_eq!(code, 0, "同一份配置在内置链必须跑通: {bound_err}");
    let bound_summary = summary(&bound);
    assert_eq!(
        bound_summary["rejected_orders"], 2,
        "整手规则必须真的挡下买入"
    );
    assert!(
        bound_summary["rejection_reasons"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|item| item["reason"].as_str().is_some_and(|v| v.contains("整手")))),
        "summary 拒单原因必须说明整手规则: {bound_summary}"
    );
    assert_eq!(bound_summary["fills"], 0);
    assert!(
        bound_summary["execution_costs"]["source"]
            .as_str()
            .is_some_and(|value| value.contains("ashare-rules:")),
        "持久摘要要说明 A 股费率来源: {bound_summary}"
    );
    assert!(
        bound_summary["model_descriptors"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|item| item.as_str().is_some_and(|v| v.starts_with("AshareRules@")))),
        "持久摘要要记录绑定的 A 股规则模型"
    );
    assert_ne!(
        plain_summary["result_hash"], bound_summary["result_hash"],
        "同一份配置在两条链上必须给出同一个 A 股口径"
    );
}

/// A bare runtime filename must resolve asset paths from the config directory before snapshotting.
#[test]
fn builtin_entry_resolves_bare_relative_runtime_config_before_snapshotting() {
    let root = temp_dir("relative-runtime-config");
    let working_directory = root.join("cwd");
    std::fs::create_dir_all(&working_directory).expect("create isolated working directory");

    let template = std::fs::read_to_string(deploy(RUNTIME)).expect("read runtime example");
    let mut config: serde_json::Value = serde_json::from_str(&template).unwrap();
    absolutize_paths(&mut config, &root);
    let config_path = working_directory.join("runtime.json");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap())
        .expect("write isolated runtime config");

    let args = [
        "backtest",
        "builtin",
        "ema_cross",
        FRAME,
        SPEC,
        "100",
        "--config",
        "runtime.json",
    ];
    let args = args
        .iter()
        .enumerate()
        .map(|(index, value)| match index {
            3 => deploy(FRAME).to_string_lossy().into_owned(),
            4 => deploy(SPEC).to_string_lossy().into_owned(),
            _ => (*value).to_string(),
        })
        .collect::<Vec<_>>();
    let (code, stdout, stderr) = run_from(&working_directory, &args);
    assert_eq!(code, 0, "bare relative runtime config must work: {stderr}");
    let summary = summary(&stdout);
    assert_eq!(summary["schema_version"], 7);
    assert_eq!(summary["instrument"], "000001.SZSE");
    assert_eq!(summary["fills"], 1);
    assert_eq!(summary["rejected_orders"], 0);
    assert!(summary["metrics"]["fees_raw"].as_i64().unwrap_or_default() > 0);
    assert!(summary["model_descriptors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|model| model.as_str().unwrap().starts_with("AShareFee@v1[")));
}

/// 成交数量合法时规则照样改结果：A 股佣金模型顶掉内置 2/5bp，费用与收益都不同。
#[test]
fn ashare_fee_model_replaces_the_builtin_cost_rates() {
    let root = temp_dir("fee");
    let config = config_with(&root.join("bound"), |_| {});
    let plain_config = config_without_ashare(&root.join("plain"));
    let (plain_code, plain, plain_err) = run(&builtin_args(Some(&plain_config), "100"));
    assert_eq!(plain_code, 0, "fee baseline must run: {plain_err}");
    assert_eq!(summary(&plain)["fills"], 2, "{plain_err}");
    let (bound_code, bound, bound_err) = run(&builtin_args(Some(&config), "100"));
    assert_eq!(bound_code, 0, "A-share fee run must run: {bound_err}");
    assert_eq!(summary(&bound)["fills"], 2, "{bound_err}");
    assert_ne!(
        summary(&plain)["result_hash"],
        summary(&bound)["result_hash"],
        "费率口径换了结果却没换，说明费用模型没接上"
    );
    assert_eq!(
        summary(&plain)["execution_costs"]["source"],
        "runtime-config-default"
    );
    assert!(
        summary(&bound)["execution_costs"]["source"]
            .as_str()
            .is_some_and(|source| source.starts_with("ashare-rules:")),
        "A 股规则生效时持久摘要应改用 A 股费率来源: {bound_err}"
    );
}

/// `enabled:false` 与"只配公司行为不配规则"都是配置面自相矛盾，两个入口同样报错。
#[test]
fn broken_ashare_sections_fail_closed_on_the_builtin_entry() {
    let root = temp_dir("fail-closed");
    let missing_calendar = config_with(&root.join("missing-calendar"), |value| {
        value["strategy"]
            .as_object_mut()
            .unwrap()
            .remove("ashare_calendar_path");
    });
    let (code, _, stderr) = run(&builtin_args(Some(&missing_calendar), "150"));
    assert_eq!(code, 2, "A 股规则缺交易日历必须在 CLI 回测入口拒绝");
    assert!(
        stderr.contains("必须提供交易日历"),
        "错误要说明缺少有界交易日清单: {stderr}"
    );

    let mut rules: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(deploy("qianxing.ashare.rules.json")).unwrap(),
    )
    .unwrap();
    rules["enabled"] = serde_json::Value::Bool(false);
    let disabled_rule = root.join("rules.disabled.json");
    std::fs::write(&disabled_rule, serde_json::to_string(&rules).unwrap()).unwrap();
    let disabled_config = config_with(&root, |value| {
        value["strategy"]["ashare_rules_path"] =
            serde_json::Value::String(disabled_rule.to_string_lossy().into_owned());
    });
    let (code, _, stderr) = run(&builtin_args(Some(&disabled_config), "150"));
    assert_eq!(code, 2, "禁用快照配进回测必须报错而不是静默按 A 股跑");
    assert!(
        stderr.contains("enabled 必须为 true"),
        "报错要说清缺的是哪一项: {stderr}"
    );

    let actions_only = config_with(&root, |value| {
        value["strategy"]
            .as_object_mut()
            .unwrap()
            .remove("ashare_rules_path");
    });
    let (code, _, stderr) = run(&builtin_args(Some(&actions_only), "150"));
    assert_eq!(code, 2, "公司行为没有规则锚定时必须拒绝");
    assert!(
        stderr.contains("必须同时配置 ashare_rules_path"),
        "配对缺失要报出键名: {stderr}"
    );
}

/// 承不了 A 股段的两条链必须当场拒绝，而不是收下配置再静默丢掉。
#[test]
fn entries_without_ashare_hooks_reject_the_config() {
    let root = temp_dir("reject");
    let config = config_with(&root, |_| {});
    let config = config.to_string_lossy().into_owned();

    let (code, _, stderr) = run(&[
        "backtest".into(),
        "book".into(),
        "--fill-tier".into(),
        "l1".into(),
        "--root".into(),
        temp_dir("book-root").to_string_lossy().into_owned(),
        "grid".into(),
        deploy("qianxing.depth-frame.example.json")
            .to_string_lossy()
            .into_owned(),
        "--config".into(),
        config.clone(),
    ]);
    assert_eq!(code, 2, "深度档没有 A 股挂钩点，收下配置就该拒");
    assert!(
        stderr.contains("深度档位回测不接受 strategy.ashare_rules_path")
            && stderr.contains("盘口引擎"),
        "拒绝要说明是哪条链、为什么: {stderr}"
    );

    let (code, _, stderr) = run(&[
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
        temp_dir("multi-root").to_string_lossy().into_owned(),
        "--config".into(),
        config,
    ]);
    assert_eq!(code, 2, "多腿链一份配置套不了两条腿，必须拒");
    assert!(
        stderr.contains("backtest multi-builtin 不接受 strategy.ashare_rules_path")
            && stderr.contains("两条腿"),
        "拒绝要说明是哪条链、为什么: {stderr}"
    );
}

/// 配对规则住在共用的加载器里，所以另一条 Bar 链（`strategy backtest`，配置走位置参数）
/// 也必须报同一个错：各自实现一份校验，就等于回到"两条链各挑一份口径"。
#[test]
fn strategy_chain_rejects_the_same_broken_pairing() {
    let root = temp_dir("pairing");
    let actions_only = config_with(&root, |value| {
        value["strategy"]
            .as_object_mut()
            .unwrap()
            .remove("ashare_rules_path");
    });
    let (code, _, stderr) = run(&[
        "strategy".into(),
        "backtest".into(),
        actions_only.to_string_lossy().into_owned(),
        deploy(FRAME).to_string_lossy().into_owned(),
        deploy(SPEC).to_string_lossy().into_owned(),
    ]);
    assert_eq!(code, 2, "公司行为没有规则锚定时必须拒绝: {stderr}");
    assert!(
        stderr.contains("必须同时配置 ashare_rules_path"),
        "策略链要报出与内置链同一条配对规则: {stderr}"
    );
}
