use super::{repo_root, scratch};
use serde_json::Value;
use std::process::Command;

/// `backtest multi-builtin` 的回归对拍：两腿结果标识、公开指标行及组合产物集合。
#[test]
fn multi_builtin_cli_preserves_stdout_and_artifacts_with_fractional_price_sizing() {
    let root = scratch("multi-builtin-cli-parity");
    let deploy = repo_root().join("deploy");
    let primary = deploy.join("qianxing.bar-frame.pairs-primary.example.json");
    let reference = deploy.join("qianxing.bar-frame.pairs-reference.example.json");
    let primary_spec = deploy.join("qianxing.market.binance.btc-swap.spec.json");
    let reference_spec = deploy.join("qianxing.market.binance.eth-swap.spec.json");
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(repo_root())
        .args(["backtest", "multi-builtin", "pairs_arbitrage"])
        .arg(primary)
        .arg(reference)
        .arg(primary_spec)
        .arg(reference_spec)
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
    assert!(
        stdout.contains("naked_filled_qty_raw=0"),
        "perpetual pair backtest must not leave one-sided fills: {stdout}"
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
    assert!(
        leg_hashes
            .iter()
            .all(|hash| hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())),
        "两条腿都应输出有效的 replay 结果哈希"
    );
    assert_ne!(leg_hashes[0], leg_hashes[1], "不同腿的回测结果应分别标识");

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
    let composition_name = entries
        .iter()
        .find(|name| name.starts_with("multi-composition") && name.ends_with(".summary.json"))
        .expect("composition summary artifact exists");
    let composition: Value = serde_json::from_slice(
        &std::fs::read(run_dir.join(composition_name)).expect("read composition summary"),
    )
    .expect("composition summary is JSON");
    let schema: Value = serde_json::from_slice(
        &std::fs::read(repo_root().join("schemas/multi-builtin-composition-summary-v7.json"))
            .expect("read composition summary schema"),
    )
    .expect("composition summary schema is JSON");
    let summary_keys = composition
        .as_object()
        .expect("composition summary object")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let schema_keys = schema["properties"]
        .as_object()
        .expect("composition summary schema properties")
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let required_keys = schema["required"]
        .as_array()
        .expect("composition schema required fields")
        .iter()
        .map(|key| key.as_str().expect("required field name").to_string())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        summary_keys.iter().all(|key| schema_keys.contains(key)),
        "actual artifact contains fields outside its schema"
    );
    assert!(
        required_keys.iter().all(|key| summary_keys.contains(key)),
        "actual artifact omits a required schema field"
    );
    assert_eq!(composition["schema_version"], 7);
    assert_eq!(composition["input"]["kind"], "multi-barframe");
    assert_eq!(composition["legs"].as_array().map(Vec::len), Some(2));
    let combined_return_bps = composition["totals"]["combined_return_bps"]
        .as_i64()
        .expect("combined return is an integer")
        .to_string();
    assert_eq!(
        composition["metrics_exact"]["equity"]["return_bps"].as_str(),
        Some(combined_return_bps.as_str())
    );
    let equity_path = composition["artifact_locations"]["equity"]
        .as_str()
        .expect("composition declares exact equity CSV");
    let equity_csv = std::fs::read_to_string(equity_path).expect("read composition equity CSV");
    assert_eq!(
        equity_csv.lines().next(),
        Some("index,ts,primary_equity_raw,reference_equity_raw,equity_raw,equity_exact")
    );
    let evidence_name = entries
        .iter()
        .find(|name| name.ends_with(".evidence.json"))
        .expect("multi-leg run publishes its EvidenceBundle");
    let evidence: Value = serde_json::from_slice(
        &std::fs::read(run_dir.join(evidence_name)).expect("read multi-leg EvidenceBundle"),
    )
    .expect("EvidenceBundle is JSON");
    let components = evidence
        .pointer("/generation/run/dataset/components")
        .and_then(Value::as_array)
        .expect("multi-leg EvidenceBundle declares its input components");
    assert_eq!(components.len(), 2);
    assert_eq!(components[0]["role"], "primary");
    assert_eq!(components[1]["role"], "reference");
    assert!(components.iter().all(|component| {
        component["row_count"].as_u64().is_some_and(|rows| rows > 0)
            && component["content_fingerprint"].as_str().is_some()
    }));
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
    assert!(
        hashes.len() == 3
            && hashes.iter().all(|hash| matches!(hash.len(), 16 | 64)
                && hash.bytes().all(|byte| byte.is_ascii_hexdigit())),
        "each leg summary and the composition summary must carry a valid replay result hash"
    );

    let _ = std::fs::remove_dir_all(root);
}
