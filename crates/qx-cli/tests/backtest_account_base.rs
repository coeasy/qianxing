//! 回测链的账户本金（V11 Q72 / 回测 FN9）：可声明、生效来源可见、算不出来的不入账。
//!
//! 走真实子进程：本金从 `--config` 那一格走到引擎账户，中间隔着读配置、折成 `Money`、
//! 装进装配、被风控门当可用现金读、最后落进摘要与 stdout —— 同进程调用复制不到这条腿。
//!
//! 缺陷原形（本轮实测）：同一份 `sma_cross` + 同一份 pairs-primary 夹具 + 同一个
//! `--quantity 2`，没声明本金时 `fills=0 return_bps=0`（钱不够，被"买入成本超过账户可用现金"
//! 挡下），声明 200,000 后 `fills=1 return_bps=-23`。前者念出来像"这策略不赚不赔"，
//! 真相是"这笔计划没人给它算过钱"。

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
    let root = std::env::temp_dir().join(format!(
        "qianxing-account-base-{label}-{}-{}",
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

fn field(stdout: &str, marker: &str, key: &str) -> String {
    stdout
        .lines()
        .find(|line| line.starts_with(marker))
        .and_then(|line| line.split(&format!("{key}=")).nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_default()
        .to_string()
}

fn line_with(stdout: &str, marker: &str) -> String {
    stdout
        .lines()
        .find(|line| line.starts_with(marker))
        .unwrap_or("")
        .to_string()
}

/// 唯一一处 100,000 默认的定点写法，用例不各自抄一份数字。
const DEFAULT_RAW: &str = "100000000000000";
/// 够买两腿夹具 2 单位的账户：2 × 63,000 + 手续费。JSON 里的整数按 i64 写即可。
const TWO_UNIT_RAW: i64 = 200_000 * 1_000_000_000;

/// 基于示例运行时配置生成用例配置：把指向 deploy 的路径绝对化，产物根改到临时目录。
///
/// `name` 决定配置文件名，`edit` 是本文件唯一的变量：每个用例只改自己那一格，其余口径与基线逐字相同。
fn runtime_with(root: &Path, name: &str, edit: impl FnOnce(&mut serde_json::Value)) -> PathBuf {
    let template =
        std::fs::read_to_string(deploy("qianxing.runtime.builtin-strategy.example.json"))
            .expect("读取示例运行时配置失败");
    let mut value: serde_json::Value = serde_json::from_str(&template).unwrap();
    value["storage"]["data_dir"] =
        serde_json::Value::String(root.join("data").to_string_lossy().into_owned());
    for key in ["bars_snapshot_path", "dataset_bundle_path"] {
        if let Some(text) = value["strategy"].get(key).and_then(|v| v.as_str()) {
            value["strategy"][key] =
                serde_json::Value::String(deploy(text).to_string_lossy().into_owned());
        }
    }
    edit(&mut value);
    // 文件名就是用例名：同一目录里写多份配置时，共用一个文件名会让后一份悄悄盖掉前一份。
    let path = root.join(format!("runtime.{name}.json"));
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).expect("写入用例配置失败");
    path
}

/// `backtest builtin` 的位置参数顺序：`<STRATEGY> <FRAME> [SPEC] [QUANTITY]`。
fn builtin_args(frame: &Path, quantity: &str, config: Option<&Path>) -> Vec<String> {
    let mut args = vec![
        "backtest".into(),
        "builtin".into(),
        "sma_cross".into(),
        frame.to_string_lossy().into_owned(),
        deploy("qianxing.binance.spot.spec.json")
            .to_string_lossy()
            .into_owned(),
        quantity.into(),
    ];
    if let Some(config) = config {
        args.extend(["--config".into(), config.to_string_lossy().into_owned()]);
    }
    args
}

/// 没声明也要把用的数印出来：这条链不落摘要，stdout 是本金唯一的出口。
#[test]
fn builtin_entry_prints_the_default_it_booked_with() {
    let (_, stdout, stderr) = run(&builtin_args(
        &deploy("qianxing.bar-frame.pairs-primary.example.json"),
        "1",
        None,
    ));
    assert_eq!(
        line_with(&stdout, "[Builtin · Account]"),
        format!("[Builtin · Account] initial_cash_raw={DEFAULT_RAW} account_base_source=builtin-default"),
        "默认本金必须原样印出来，缺了这行等于回到「没人知道自己跑在多少钱上」: {stderr}"
    );
    assert_eq!(field(&stdout, "[Builtin · Backtest]", "fills"), "1");
}

/// 同一份配置、同一份夹具、同一个下单量：只有那一格本金不同，结果就必须不同。
#[test]
fn a_declared_account_base_is_what_makes_the_second_unit_fillable() {
    let root = temp_dir("result-changing");
    let frame = deploy("qianxing.bar-frame.pairs-primary.example.json");
    let undeclared = runtime_with(&root, "undeclared", |_| {});
    let declared = runtime_with(&root, "declared", |value| {
        value["strategy"]["initial_cash_raw"] = serde_json::Value::from(TWO_UNIT_RAW);
    });

    let (code, plain, plain_err) = run(&builtin_args(&frame, "2", Some(&undeclared)));
    assert_eq!(code, 0, "不声明本金的基线要能跑完: {plain_err}");
    assert_eq!(field(&plain, "[Builtin · Backtest]", "fills"), "0");
    assert_eq!(field(&plain, "[Builtin · Backtest]", "return_bps"), "0");
    assert!(
        line_with(&plain, "[Builtin · Integrity]").contains("超过账户可用现金"),
        "零成交的理由要说清是钱不够: {}",
        line_with(&plain, "[Builtin · Integrity]")
    );
    assert!(
        line_with(&plain, "[Builtin · Account]").contains("account_base_source=builtin-default"),
        "没声明时来源得写成默认，不能与「配了同样的数」混: {}",
        line_with(&plain, "[Builtin · Account]")
    );

    let (code, funded, funded_err) = run(&builtin_args(&frame, "2", Some(&declared)));
    assert_eq!(code, 0, "声明本金后同一条命令要能跑完: {funded_err}");
    assert_eq!(
        field(&funded, "[Builtin · Backtest]", "fills"),
        "1",
        "声明的本金必须真的进到引擎账户"
    );
    assert!(
        line_with(&funded, "[Builtin · Account]")
            .contains("account_base_source=strategy-initial-cash"),
        "配置来源与默认来源必须可区分: {}",
        line_with(&funded, "[Builtin · Account]")
    );
    assert!(
        line_with(&funded, "[Builtin · Account]")
            .contains(&format!("initial_cash_raw={TWO_UNIT_RAW}")),
        "印出来的必须是生效的那个数: {}",
        line_with(&funded, "[Builtin · Account]")
    );
    assert_ne!(
        field(&plain, "[Builtin · Backtest]", "result_hash"),
        field(&funded, "[Builtin · Backtest]", "result_hash"),
        "两腿之差全在本金上，结果哈希却是同一个说明声明没生效"
    );
}

/// 非正的声明报错，不回落默认：0 元账户上的 `return_bps=0` 是一句假话。
#[test]
fn a_non_positive_declaration_fails_closed() {
    let root = temp_dir("non-positive");
    let frame = deploy("qianxing.bar-frame.pairs-primary.example.json");
    for raw in [0, -1] {
        let config = runtime_with(&root, "non-positive", |value| {
            value["strategy"]["initial_cash_raw"] = serde_json::Value::from(raw);
        });
        let (code, _, stderr) = run(&builtin_args(&frame, "1", Some(&config)));
        assert_eq!(code, 2, "非法本金必须当场拒绝（{raw}）");
        assert!(
            stderr.contains(&format!("strategy.initial_cash_raw={raw}"))
                && stderr.contains("不是正的本金"),
            "报错要格出键名、值与理由: {stderr}"
        );
    }
}

/// 落摘要的两条链（`strategy backtest` / `backtest book`）把本金与来源写进产物。
#[test]
fn summary_chains_land_the_account_base_they_actually_used() {
    let root = temp_dir("summaries");
    let declared = runtime_with(&root, "declared", |value| {
        value["strategy"]["initial_cash_raw"] = serde_json::Value::from(TWO_UNIT_RAW);
    });
    let (code, stdout, stderr) = run(&[
        "strategy".into(),
        "backtest".into(),
        declared.to_string_lossy().into_owned(),
        deploy("qianxing.bar-frame.example.json")
            .to_string_lossy()
            .into_owned(),
        deploy("qianxing.binance.spot.spec.json")
            .to_string_lossy()
            .into_owned(),
    ]);
    assert_eq!(code, 0, "策略链声明本金后要跑通: {stderr}");
    assert!(
        line_with(&stdout, "[Strategy · Account]").contains("strategy-initial-cash"),
        "策略链同样要把来源印出来: {}",
        line_with(&stdout, "[Strategy · Account]")
    );
    let summary = first_summary(&root.join("data").join("runs"));
    assert_eq!(
        summary["account"],
        serde_json::json!({
            "initial_cash_raw": TWO_UNIT_RAW.to_string(),
            "source": "strategy-initial-cash",
        }),
        "摘要里的本金必须等于真正记账的那一个: {summary}"
    );
    assert_eq!(
        summary["schema_version"],
        serde_json::json!(4),
        "多了一个每期本金键，摘要世代必须随之上行"
    );

    let book_root = temp_dir("summaries-book");
    let (code, stdout, stderr) = run(&[
        "backtest".into(),
        "book".into(),
        "--fill-tier".into(),
        "l1".into(),
        "--root".into(),
        book_root.to_string_lossy().into_owned(),
        "grid".into(),
        deploy("qianxing.depth-frame.l1.example.json")
            .to_string_lossy()
            .into_owned(),
        "--config".into(),
        declared.to_string_lossy().into_owned(),
    ]);
    assert_eq!(code, 0, "深度链声明本金后要跑通: {stderr}");
    assert!(
        line_with(&stdout, "[Depth · Account]").contains(&TWO_UNIT_RAW.to_string()),
        "深度链同样要印出生效的本金: {}",
        line_with(&stdout, "[Depth · Account]")
    );
    let depth_summary = first_summary(&book_root.join("runs"));
    assert_eq!(
        depth_summary["account"]["source"],
        serde_json::Value::String("strategy-initial-cash".into()),
        "深度链的本金来源也要落盘: {depth_summary}"
    );
    assert_eq!(
        depth_summary["account"]["initial_cash_raw"],
        serde_json::Value::String(TWO_UNIT_RAW.to_string()),
        "深度链不得停在常数字面上: {depth_summary}"
    );
}

/// 多腿链一份配置套不了两条腿：声明了就必须拒，同时产物要写出自己的本金从哪来。
#[test]
fn the_multi_leg_entry_refuses_a_single_account_number_and_names_its_own_rule() {
    let root = temp_dir("multi-leg");
    let declared = runtime_with(&root, "declared", |value| {
        value["strategy"]["initial_cash_raw"] = serde_json::Value::from(TWO_UNIT_RAW);
    });
    let primary = deploy("qianxing.bar-frame.pairs-primary.example.json");
    let reference = deploy("qianxing.bar-frame.pairs-reference.example.json");
    let args = |config: Option<&Path>| -> Vec<String> {
        let mut args = vec![
            "backtest".into(),
            "multi-builtin".into(),
            "pairs_arbitrage".into(),
            primary.to_string_lossy().into_owned(),
            reference.to_string_lossy().into_owned(),
            "--root".into(),
            root.to_string_lossy().into_owned(),
        ];
        if let Some(config) = config {
            args.extend(["--config".into(), config.to_string_lossy().into_owned()]);
        }
        args
    };
    let (code, _, stderr) = run(&args(Some(&declared)));
    assert_eq!(
        code, 2,
        "一份账户数字定不了两条腿的本金，收下再丢就是配置说假话"
    );
    assert!(
        stderr.contains("backtest multi-builtin 不接受 strategy.initial_cash_raw")
            && stderr.contains("两条腿"),
        "拒绝要说明是哪条链、为什么: {stderr}"
    );

    let (code, _, stderr) = run(&args(None));
    assert_eq!(code, 0, "不声明时多腿链照常跑: {stderr}");
    let attribution = std::fs::read_dir(root.join("runs"))
        .unwrap()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            path.file_name()?
                .to_string_lossy()
                .ends_with(".spread-attribution.json")
                .then_some(path)
        })
        .collect::<Vec<_>>();
    assert_eq!(attribution.len(), 1, "应产出一份归因产物");
    let payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&attribution[0]).unwrap()).unwrap();
    assert_eq!(
        payload["accounts"]["account_base_source"], "multi-leg-funding-rule",
        "四条链都要交代本金来源，多腿的答案是定资规则: {payload}"
    );
}

fn first_summary(runs_dir: &Path) -> serde_json::Value {
    let path = std::fs::read_dir(runs_dir)
        .unwrap_or_else(|error| panic!("读取回测产物目录失败 {}: {error}", runs_dir.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".summary.json"))
        })
        .expect("回测摘要未落盘");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}
