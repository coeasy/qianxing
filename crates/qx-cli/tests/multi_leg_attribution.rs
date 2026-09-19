//! 多腿（配对套利）组级归因的端到端链路测试：费用闭合、保证金/资金费口径与确定性。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("仓库根目录")
        .join("deploy")
        .join(name)
        .to_string_lossy()
        .to_string()
}

fn temp_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "qianxing-multi-leg-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("创建多腿归因产物目录失败");
    root
}

fn run(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(args)
        .output()
        .expect("启动 qx-cli 多腿回测失败");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// 用给定标的（可选衍生品规格）跑一次多腿回测，`--root` 落在 `out` 下。
fn backtest(out: &Path, quantity: &str, funding_bps: &str, legs: &[&str]) -> (i32, String, String) {
    let root = out.to_string_lossy().to_string();
    let mut args = vec!["backtest", "multi-builtin", "pairs_arbitrage"];
    args.extend(legs.iter().copied());
    args.extend([
        "--quantity",
        quantity,
        "--funding-bps",
        funding_bps,
        "--root",
        root.as_str(),
    ]);
    run(&args)
}

fn attribution_fields(stdout: &str) -> BTreeMap<String, String> {
    let line = stdout
        .lines()
        .find(|line| line.starts_with("[Multi-leg · Attribution]"))
        .expect("输出缺少 [Multi-leg · Attribution]");
    line.split_whitespace()
        .filter_map(|token| token.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn result_hashes(stdout: &str) -> String {
    stdout
        .lines()
        .find(|line| line.starts_with("[Multi-leg · Backtest]"))
        .and_then(|line| line.split("result_hashes=").nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_string)
        .expect("输出缺少 [Multi-leg · Backtest] result_hashes")
}

fn attribution_artifact(root: &Path) -> serde_json::Value {
    let runs = root.join("runs");
    let path = std::fs::read_dir(&runs)
        .expect("读取 runs 目录失败")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".spread-attribution.json"))
        })
        .expect("缺少多腿归因产物");
    let payload = std::fs::read_to_string(&path).expect("读取多腿归因产物失败");
    serde_json::from_str(&payload).expect("多腿归因产物 JSON 非法")
}

fn field(fields: &BTreeMap<String, String>, key: &str) -> i128 {
    fields
        .get(key)
        .unwrap_or_else(|| panic!("归因摘要缺少 {key}"))
        .parse::<i128>()
        .unwrap_or_else(|_| panic!("归因摘要 {key} 不是整数"))
}

/// 从 CLI 产物里取出一组组归因，并交给领域类型复核合计一致性。
fn validated_groups(payload: &serde_json::Value) -> Vec<qx_zhenlu::SpreadGroupAttribution> {
    payload["groups"]
        .as_array()
        .expect("groups 必须是数组")
        .iter()
        .map(|group| {
            qx_zhenlu::SpreadGroupAttribution::from_json(&group.to_string())
                .unwrap_or_else(|error| panic!("组级归因未通过领域校验: {error}"))
        })
        .collect()
}

#[test]
fn spot_multi_leg_attribution_closes_fees_and_stays_deterministic() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    let legs = [primary.as_str(), reference.as_str()];
    let root = temp_root("spot");
    let first = root.join("first");
    let (code, stdout, stderr) = backtest(&first, "2", "30", &legs);
    assert_eq!(code, 0, "现货多腿回测失败: {stderr}");
    let fields = attribution_fields(&stdout);
    let turnover_raw = field(&fields, "turnover_raw");
    let fees_raw = field(&fields, "fees_raw");
    assert_eq!(field(&fields, "groups"), 3);
    assert_eq!(field(&fields, "residual_filled_qty_raw"), 0);
    assert_eq!(field(&fields, "residual_fees_raw"), 0);
    // 现货没有保证金与资金费；费用必须是名义额的 taker 5bp，而不是 1e9 分之一。
    assert_eq!(field(&fields, "margin_peak_raw"), 0);
    assert_eq!(field(&fields, "funding_raw"), 0);
    assert_eq!(turnover_raw, 385_200_000_000_000);
    assert_eq!(fees_raw, turnover_raw * 5 / 10_000);
    assert_eq!(field(&fields, "net_cost_raw"), fees_raw);
    assert_eq!(field(&fields, "cost_bps"), 5);

    let payload = attribution_artifact(&first);
    assert_eq!(payload["schema_version"], 1);
    assert_eq!(payload["strategy_id"], "builtin-pairs_arbitrage-multi");
    assert_eq!(payload["totals"]["fees_raw"], fees_raw.to_string());
    assert_eq!(payload["totals"]["turnover_raw"], turnover_raw.to_string());
    assert_eq!(payload["totals"]["residual_fees_raw"], "0");
    let groups = validated_groups(&payload);
    assert_eq!(groups.len(), 3);
    let summed_fees: i128 = groups
        .iter()
        .map(|group| group.legs.iter().map(|leg| leg.fees_raw).sum::<i128>())
        .sum();
    assert_eq!(
        summed_fees, fees_raw,
        "腿级费用合计必须等于摘要费用，FIFO 分配不得漏计"
    );
    assert!(groups.iter().all(|group| group.legs.len() == 2));
    assert!(groups
        .iter()
        .all(|group| group.total_fees_raw > 0 && group.total_turnover_raw > 0));

    let second = root.join("second");
    let (code, second_stdout, _) = backtest(&second, "2", "30", &legs);
    assert_eq!(code, 0);
    assert_eq!(
        result_hashes(&second_stdout),
        result_hashes(&stdout),
        "同参数二次回测结果指纹漂移"
    );
    assert_eq!(attribution_fields(&second_stdout), fields);

    let changed = root.join("changed");
    let (code, changed_stdout, _) = backtest(&changed, "1", "30", &legs);
    assert_eq!(code, 0);
    assert_ne!(
        result_hashes(&changed_stdout),
        result_hashes(&stdout),
        "数量变化必须改变结果指纹"
    );
    assert_eq!(
        field(&attribution_fields(&changed_stdout), "filled_qty_raw"),
        6_000_000_000
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn derivative_multi_leg_attribution_charges_margin_and_funding_on_realized_fills() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    let primary_spec = fixture("qianxing.market.binance.btc-swap.spec.json");
    let reference_spec = fixture("qianxing.market.binance.eth-swap.spec.json");
    let legs = [
        primary.as_str(),
        reference.as_str(),
        primary_spec.as_str(),
        reference_spec.as_str(),
    ];
    let root = temp_root("swap");
    let margined = root.join("margined");
    let (code, stdout, stderr) = backtest(&margined, "1", "30", &legs);
    assert_eq!(code, 0, "衍生品多腿回测失败: {stderr}");
    let fields = attribution_fields(&stdout);
    // 两条腿都成交：保证金按 |实际成交持仓| × bar 收盘 × 1 倍杠杆。
    assert_eq!(field(&fields, "margin_peak_raw"), 64_200_000_000_000);
    assert_eq!(field(&fields, "funding_raw"), -39_600_000_000);
    assert_eq!(field(&fields, "residual_fees_raw"), 0);
    assert_eq!(field(&fields, "residual_filled_qty_raw"), 0);

    let zero_funding = root.join("zero-funding");
    let (code, zero_stdout, _) = backtest(&zero_funding, "1", "0", &legs);
    assert_eq!(code, 0);
    let zero_fields = attribution_fields(&zero_stdout);
    assert_eq!(field(&zero_fields, "funding_raw"), 0);
    assert_eq!(
        field(&zero_fields, "margin_peak_raw"),
        field(&fields, "margin_peak_raw"),
        "资金费率不应改变保证金口径"
    );
    assert_eq!(
        field(&zero_fields, "net_cost_raw"),
        field(&zero_fields, "fees_raw"),
        "资金费为 0 时净成本等于费用"
    );

    let doubled = root.join("doubled");
    let (code, doubled_stdout, _) = backtest(&doubled, "1", "60", &legs);
    assert_eq!(code, 0);
    let doubled_fields = attribution_fields(&doubled_stdout);
    assert_eq!(
        field(&doubled_fields, "funding_raw"),
        field(&fields, "funding_raw") * 2,
        "资金费必须随费率线性缩放"
    );
    assert_eq!(
        field(&doubled_fields, "fees_raw"),
        field(&fields, "fees_raw"),
        "资金费率不得改变费用合计"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn multi_leg_attribution_rejects_unknown_flags() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    let (code, _, stderr) = run(&[
        "backtest",
        "multi-builtin",
        "pairs_arbitrage",
        primary.as_str(),
        reference.as_str(),
        "--typo",
    ]);
    assert_eq!(code, 2, "未知参数没有被拒绝: {stderr}");
    assert!(stderr.contains("未知参数"), "报错未指出未知参数: {stderr}");
}
