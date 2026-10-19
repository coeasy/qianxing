use super::{repo_root, scratch};
use serde_json::Value;
use std::process::Command;

/// `backtest multi-builtin` 的迁移对拍：固定旧路径结果哈希、公开指标行及完整组合产物集合。
#[test]
fn multi_builtin_cli_matches_the_legacy_hash_stdout_and_artifact_contract() {
    let root = scratch("multi-builtin-cli-parity");
    let deploy = repo_root().join("deploy");
    let primary = deploy.join("qianxing.bar-frame.pairs-primary.example.json");
    let reference = deploy.join("qianxing.bar-frame.pairs-reference.example.json");
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(repo_root())
        .args(["backtest", "multi-builtin", "pairs_arbitrage"])
        .arg(primary)
        .arg(reference)
        .args(["--root"])
        .arg(&root)
        .output()
        .expect("start multi-builtin CLI parity scenario");

    assert_eq!(
        output.status.code(),
        Some(0),
        "multi-builtin CLI must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let backtest = stdout
        .lines()
        .find(|line| line.starts_with("[Multi-leg · Backtest]"))
        .expect("supported backtest stdout contract line");
    let fields = |line: &str| {
        line.split_whitespace()
            .skip(2)
            .filter_map(|field| field.split_once('='))
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let backtest = fields(backtest);
    assert_eq!(
        backtest.get("strategy").map(String::as_str),
        Some("pairs_arbitrage")
    );
    assert_eq!(
        backtest.get("primary").map(String::as_str),
        Some("BTCUSDT.BINANCE")
    );
    assert_eq!(
        backtest.get("reference").map(String::as_str),
        Some("ETHUSDT.BINANCE")
    );
    assert_eq!(
        backtest.get("combined_return_bps").map(String::as_str),
        Some("-110")
    );
    let mut leg_hashes = backtest
        .get("result_hashes")
        .expect("backtest output declares both leg hashes")
        .split('/')
        .collect::<Vec<_>>();
    leg_hashes.sort_unstable();
    assert_eq!(leg_hashes, ["4674f7667f4ead02", "4e40f0d5e8e79687"]);

    let attribution = stdout
        .lines()
        .find(|line| line.starts_with("[Multi-leg · Attribution]"))
        .expect("supported attribution stdout contract line");
    let attribution = fields(attribution);
    assert_eq!(attribution.get("groups").map(String::as_str), Some("3"));
    assert_eq!(
        attribution
            .get("residual_filled_qty_raw")
            .map(String::as_str),
        Some("0")
    );

    let run_dir = root.join("runs");
    let entries = std::fs::read_dir(&run_dir)
        .unwrap_or_else(|error| panic!("read CLI artifact set {}: {error}", run_dir.display()))
        .map(|entry| {
            entry
                .expect("read artifact entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<Vec<_>>();
    let mut artifact_suffixes = entries
        .iter()
        .map(|name| {
            name.rsplit_once('.')
                .map(|(_, suffix)| suffix)
                .unwrap_or("")
        })
        .collect::<Vec<_>>();
    artifact_suffixes.sort_unstable();
    assert_eq!(
        artifact_suffixes,
        [
            "csv", "csv", "csv", "csv", "csv", "csv", "json", "json", "json", "json", "json",
            "json", "json", "json", "json", "json", "json",
        ],
        "two leg bundles, composition bundle, evidence bundle and spread attribution produce 17 files"
    );
    let mut hashes = entries
        .iter()
        .filter(|name| name.ends_with(".summary.json"))
        .map(|name| {
            let payload: Value = serde_json::from_slice(
                &std::fs::read(run_dir.join(name)).expect("read summary artifact"),
            )
            .expect("summary artifact is JSON");
            payload["result_hash"]
                .as_str()
                .expect("summary carries result_hash")
                .to_string()
        })
        .collect::<Vec<_>>();
    hashes.sort_unstable();
    assert_eq!(
        hashes,
        [
            "4674f7667f4ead02",
            "4e40f0d5e8e79687",
            "5b408f7f29ecfd26bbe6b3ea6af8869f3b544acfb52be3a6536a5cf801978146",
        ],
        "CLI artifact result_hash multiset must preserve the pre-migration baseline"
    );

    let _ = std::fs::remove_dir_all(root);
}
