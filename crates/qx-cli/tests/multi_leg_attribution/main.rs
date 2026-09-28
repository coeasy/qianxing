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
    line_fields(stdout, "[Multi-leg · Attribution]")
}

fn integrity_fields(stdout: &str) -> BTreeMap<String, String> {
    line_fields(stdout, "[Multi-leg · Integrity]")
}

fn reconcile_fields(stdout: &str) -> BTreeMap<String, String> {
    line_fields(stdout, "[Multi-leg · Reconcile]")
}

fn line_fields(stdout: &str, prefix: &str) -> BTreeMap<String, String> {
    let line = stdout
        .lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("输出缺少 {prefix}"));
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

/// 指定策略 kind 与额外旗标跑一次多腿回测。
fn multi_backtest(
    out: &Path,
    kind: &str,
    quantity: &str,
    funding_bps: &str,
    legs: &[&str],
    extra: &[&str],
) -> (i32, String, String) {
    let root = out.to_string_lossy().to_string();
    let mut args = vec!["backtest", "multi-builtin", kind];
    args.extend(legs.iter().copied());
    args.extend([
        "--quantity",
        quantity,
        "--funding-bps",
        funding_bps,
        "--root",
        root.as_str(),
    ]);
    args.extend(extra.iter().copied());
    run(&args)
}

mod entries;
mod margin_and_return;
