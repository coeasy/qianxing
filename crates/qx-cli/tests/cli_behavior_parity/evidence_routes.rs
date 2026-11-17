use super::{artifact_digests, artifact_files, binary, temp_dir};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

#[derive(Debug, PartialEq, Eq)]
struct FailureObservation {
    exit_code: i32,
    stdout: String,
    stderr: String,
    artifacts: Vec<String>,
    artifact_digests: Vec<(String, u64)>,
}

fn observe_failure(args: &[&str], cwd: &Path) -> FailureObservation {
    let output = Command::new(binary())
        .current_dir(cwd)
        .env_remove("QX_DEPLOY_DIR")
        .env_remove("QX_PYTHON")
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run qx-cli {args:?}: {error}"));
    let cwd_text = cwd.to_string_lossy();
    let stderr = String::from_utf8_lossy(&output.stderr)
        .replace(cwd_text.as_ref(), "<workdir>")
        .replace(&cwd_text.replace('\\', "/"), "<workdir>")
        .replace(&cwd_text.replace('/', "\\"), "<workdir>");
    let artifacts = artifact_files(cwd);
    FailureObservation {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr,
        artifact_digests: artifact_digests(cwd, &artifacts),
        artifacts,
    }
}

#[test]
fn evidence_subcommands_replay_missing_input_failures_without_side_effects() {
    let cases: [(&str, &[&str]); 26] = [
        (
            "sign",
            &[
                "evidence",
                "sign",
                "missing-bundle.json",
                "--signing-key-file",
                "missing-key.bin",
                "--signature-file",
                "signature.json",
            ],
        ),
        (
            "verify",
            &[
                "evidence",
                "verify",
                "missing-bundle.json",
                "--signature-file",
                "missing-signature.json",
                "--trusted-key-file",
                "missing-public-key.bin",
            ],
        ),
        (
            "pack-venue-acceptance",
            &[
                "evidence",
                "pack-venue-acceptance",
                "missing-acceptance-dir",
                "--operator",
                "replay-test",
            ],
        ),
        (
            "verify-run",
            &["evidence", "verify-run", "missing-run-evidence.json"],
        ),
        (
            "export-run",
            &[
                "evidence",
                "export-run",
                "missing-run-evidence.json",
                "--output-dir",
                "portable-output",
            ],
        ),
        (
            "pack-multi-market-run",
            &[
                "evidence",
                "pack-multi-market-run",
                "missing-market-run.json",
            ],
        ),
        (
            "pack-multi-account-run",
            &[
                "evidence",
                "pack-multi-account-run",
                "missing-multi-account-result.json",
                "--spec",
                "missing-multi-account-spec.json",
                "--output-dir",
                "portable-output",
            ],
        ),
        (
            "verify-multi-account-run",
            &[
                "evidence",
                "verify-multi-account-run",
                "missing-portable-multi-account-dir",
            ],
        ),
        (
            "pack-multi-account-depth-run",
            &[
                "evidence",
                "pack-multi-account-depth-run",
                "missing-multi-account-depth-result.json",
                "--spec",
                "missing-multi-account-depth-spec.json",
                "--output-dir",
                "portable-output",
            ],
        ),
        (
            "pack-multi-account-joint-run",
            &[
                "evidence",
                "pack-multi-account-joint-run",
                "missing-multi-account-joint-result.json",
                "--spec",
                "missing-multi-account-joint-spec.json",
                "--output-dir",
                "portable-output",
            ],
        ),
        (
            "verify-multi-account-joint-run",
            &[
                "evidence",
                "verify-multi-account-joint-run",
                "missing-portable-multi-account-joint-dir",
            ],
        ),
        (
            "verify-multi-account-depth-run",
            &[
                "evidence",
                "verify-multi-account-depth-run",
                "missing-portable-multi-account-depth-dir",
            ],
        ),
        (
            "recompute-experiment",
            &[
                "evidence",
                "recompute-experiment",
                "missing-experiment.json",
            ],
        ),
        (
            "recompute-depth-experiment",
            &[
                "evidence",
                "recompute-depth-experiment",
                "missing-depth-experiment.json",
            ],
        ),
        (
            "pack-experiment",
            &[
                "evidence",
                "pack-experiment",
                "missing-experiment.json",
                "--output-dir",
                "portable-output",
            ],
        ),
        (
            "pack-depth-experiment",
            &[
                "evidence",
                "pack-depth-experiment",
                "missing-depth-experiment.json",
                "--output-dir",
                "portable-output",
            ],
        ),
        (
            "verify-experiment",
            &[
                "evidence",
                "verify-experiment",
                "missing-portable-experiment",
            ],
        ),
        (
            "verify-depth-experiment",
            &[
                "evidence",
                "verify-depth-experiment",
                "missing-portable-depth-experiment",
            ],
        ),
        (
            "recompute-multi-market-experiment",
            &[
                "evidence",
                "recompute-multi-market-experiment",
                "missing-market-experiment.json",
            ],
        ),
        (
            "recompute-multi-market-depth-experiment",
            &[
                "evidence",
                "recompute-multi-market-depth-experiment",
                "missing-market-depth-experiment.json",
            ],
        ),
        (
            "recompute-multi-leg-experiment",
            &[
                "evidence",
                "recompute-multi-leg-experiment",
                "missing-multi-leg-experiment.json",
            ],
        ),
        (
            "pack-multi-leg-experiment",
            &[
                "evidence",
                "pack-multi-leg-experiment",
                "missing-multi-leg-experiment.json",
                "--output-dir",
                "portable-output",
            ],
        ),
        (
            "verify-multi-leg-experiment",
            &[
                "evidence",
                "verify-multi-leg-experiment",
                "missing-portable-multi-leg-dir",
            ],
        ),
        (
            "pack-multi-market-experiment",
            &[
                "evidence",
                "pack-multi-market-experiment",
                "missing-market-experiment.json",
                "--output-dir",
                "portable-output",
            ],
        ),
        (
            "pack-multi-market-depth-experiment",
            &[
                "evidence",
                "pack-multi-market-depth-experiment",
                "missing-market-depth-experiment.json",
                "--output-dir",
                "portable-output",
            ],
        ),
        (
            "verify-multi-market-experiment",
            &[
                "evidence",
                "verify-multi-market-experiment",
                "missing-portable-market-experiment-dir",
            ],
        ),
    ];

    for (name, args) in cases {
        let first_cwd = temp_dir(&format!("evidence-{name}"), 1);
        let first = observe_failure(args, &first_cwd);
        std::fs::remove_dir_all(&first_cwd).expect("remove first evidence replay");

        let second_cwd = temp_dir(&format!("evidence-{name}"), 2);
        let second = observe_failure(args, &second_cwd);
        std::fs::remove_dir_all(&second_cwd).expect("remove second evidence replay");

        assert_eq!(
            first.exit_code, 2,
            "evidence {name} must reject missing input"
        );
        assert!(
            !first.stderr.trim().is_empty(),
            "evidence {name} needs a diagnostic"
        );
        assert!(
            first.stdout.trim().is_empty(),
            "evidence {name} must not emit partial stdout"
        );
        assert!(
            first.artifacts.is_empty(),
            "evidence {name} wrote partial output"
        );
        assert_eq!(
            first, second,
            "evidence {name} changed across clean replays"
        );
    }
}

#[test]
fn single_market_bar_and_depth_experiments_recompute_all_candidates() {
    let root = temp_dir("single-market-experiment-recompute", 1);
    let bars_path = root.join("bars.json");
    let prices = (0..48)
        .map(|index| {
            let phase = index % 20;
            if phase < 10 {
                100 + phase * 2
            } else {
                100 + (20 - phase) * 2
            }
        })
        .collect::<Vec<i64>>();
    let scale = 1_000_000_000_i64;
    std::fs::write(
        &bars_path,
        json!({
            "schema_version": 1,
            "instrument": "BTCUSDT.BINANCE",
            "source": "qx-cli-single-market-recompute-test",
            "ts": (0..prices.len()).map(|index| 1000 + index as u64 * 1000).collect::<Vec<_>>(),
            "open_raw": prices.iter().map(|price| price * scale).collect::<Vec<_>>(),
            "high_raw": prices.iter().map(|price| (price + 1) * scale).collect::<Vec<_>>(),
            "low_raw": prices.iter().map(|price| (price - 1) * scale).collect::<Vec<_>>(),
            "close_raw": prices.iter().map(|price| price * scale).collect::<Vec<_>>(),
            "volume_raw": vec![scale; prices.len()],
        })
        .to_string(),
    )
    .expect("write BarFrame");
    let bar_spec_path = root.join("bar-experiment.json");
    std::fs::write(
        &bar_spec_path,
        json!({
            "schema_version": 1,
            "experiment_id": "cli-bar-recompute",
            "base": {
                "schema_version": 1,
                "run_id": "unused-cli-bar-recompute",
                "instrument": "BTCUSDT.BINANCE",
                "bars_path": bars_path,
                "settlement_currency": "USDT",
                "initial_cash_raw": 100_000_000_000_000_i128,
                "seed": 17,
                "output_dir": root.join("bar-runs"),
                "strategy": {
                    "kind": "sma_cross",
                    "strategy_id": "cli-bar-recompute",
                    "quantity_raw": 1_000_000_000_i128,
                    "fast_window": 2,
                    "slow_window": 3
                }
            },
            "parameter_space": [{ "name": "fast_window", "values": [2] }]
        })
        .to_string(),
    )
    .expect("write Bar experiment spec");
    let bar_run = Command::new(binary())
        .current_dir(&root)
        .args(["app", "run-experiment"])
        .arg(&bar_spec_path)
        .output()
        .expect("run Bar experiment");
    assert!(
        bar_run.status.success(),
        "Bar experiment failed: {}",
        String::from_utf8_lossy(&bar_run.stderr)
    );
    let bar_report: Value = serde_json::from_slice(&bar_run.stdout).expect("Bar report JSON");
    let bar_artifact = bar_report["artifact_path"]
        .as_str()
        .expect("Bar report path");
    let bar_recomputed = Command::new(binary())
        .current_dir(&root)
        .args(["evidence", "recompute-experiment", bar_artifact])
        .output()
        .expect("recompute Bar experiment");
    assert!(
        bar_recomputed.status.success(),
        "Bar recompute failed: {}",
        String::from_utf8_lossy(&bar_recomputed.stderr)
    );
    assert!(String::from_utf8_lossy(&bar_recomputed.stdout).contains("全部 1 个候选已复算一致"));

    let bar_bundle = root.join("bar-portable");
    let bar_packed = Command::new(binary())
        .current_dir(&root)
        .args(["evidence", "pack-experiment", bar_artifact, "--output-dir"])
        .arg(&bar_bundle)
        .output()
        .expect("pack Bar experiment");
    assert!(
        bar_packed.status.success(),
        "Bar package failed: {}",
        String::from_utf8_lossy(&bar_packed.stderr)
    );
    let moved_root = root.join("relocated");
    std::fs::create_dir(&moved_root).expect("create relocated package directory");
    let moved_bar_bundle = moved_root.join("bar-portable");
    std::fs::rename(&bar_bundle, &moved_bar_bundle).expect("move Bar package");
    std::fs::remove_file(&bars_path).expect("hide original Bar input");
    std::fs::rename(root.join("bar-runs"), root.join("bar-runs-hidden"))
        .expect("hide original Bar run artifacts");
    let foreign_cwd = root.join("foreign-cwd");
    std::fs::create_dir(&foreign_cwd).expect("create foreign working directory");
    let bar_bundle_check = Command::new(binary())
        .current_dir(&foreign_cwd)
        .args(["evidence", "verify-experiment"])
        .arg(&moved_bar_bundle)
        .arg("--recompute")
        .output()
        .expect("verify relocated Bar experiment package");
    assert!(
        bar_bundle_check.status.success(),
        "relocated Bar package failed: {}",
        String::from_utf8_lossy(&bar_bundle_check.stderr)
    );
    assert!(String::from_utf8_lossy(&bar_bundle_check.stdout).contains("整网格复算一致"));
    let packaged_bars = moved_bar_bundle.join("inputs").join("bars.json");
    let packaged_bars_bytes = std::fs::read(&packaged_bars).expect("read packaged Bar input");
    let mut tampered_bars = packaged_bars_bytes.clone();
    tampered_bars.push(b' ');
    std::fs::write(&packaged_bars, tampered_bars).expect("tamper packaged Bar input");
    let tampered_check = Command::new(binary())
        .current_dir(&foreign_cwd)
        .args(["evidence", "verify-experiment"])
        .arg(&moved_bar_bundle)
        .output()
        .expect("reject tampered Bar package");
    assert_eq!(tampered_check.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&tampered_check.stderr).contains("SHA-256 不匹配"));
    std::fs::write(&packaged_bars, packaged_bars_bytes).expect("restore packaged Bar input");

    let depth_path = root.join("depth.json");
    let depth_snapshots = (0..48)
        .map(|index| {
            let phase = index % 20;
            let mid = if phase < 10 {
                100 + phase * 2
            } else {
                100 + (20 - phase) * 2
            };
            let levels = |offset: i64| {
                (0..1)
                    .map(|level| {
                        json!({
                            "price": (mid + offset * (level + 1)) * scale,
                            "qty": 100 * scale
                        })
                    })
                    .collect::<Vec<_>>()
            };
            json!({
                "instrument": { "symbol": "BTCUSDT", "venue": "BINANCE" },
                "ts": 1000 + index as u64 * 1000,
                "sequence": index as u64 + 1,
                "bids": levels(-1),
                "asks": levels(1)
            })
        })
        .collect::<Vec<_>>();
    std::fs::write(
        &depth_path,
        json!({
            "schema_version": 1,
            "source": "qx-cli-single-market-recompute-test",
            "instrument": { "symbol": "BTCUSDT", "venue": "BINANCE" },
            "snapshots": depth_snapshots
        })
        .to_string(),
    )
    .expect("write DepthFrame");
    let depth_spec_path = root.join("depth-experiment.json");
    std::fs::write(
        &depth_spec_path,
        json!({
            "schema_version": 1,
            "experiment_id": "cli-depth-recompute",
            "base": {
                "schema_version": 1,
                "run_id": "unused-cli-depth-recompute",
                "depth_path": depth_path,
                "tier": "l1",
                "settlement_currency": "USDT",
                "initial_cash_raw": 100_000_000_000_000_i128,
                "fee_bps": 0,
                "latency_snapshots": 0,
                "queue_position_bps": 0,
                "market_impact_bps": 0,
                "output_dir": root.join("depth-runs"),
                "strategy": {
                    "kind": "sma_cross",
                    "strategy_id": "cli-depth-recompute",
                    "quantity_raw": 1_000_000_000_i128,
                    "fast_window": 2,
                    "slow_window": 3
                }
            },
            "parameter_space": [{ "name": "fast_window", "values": [2] }]
        })
        .to_string(),
    )
    .expect("write depth experiment spec");
    let depth_run = Command::new(binary())
        .current_dir(&root)
        .args(["app", "run-depth-experiment"])
        .arg(&depth_spec_path)
        .output()
        .expect("run depth experiment");
    assert!(
        depth_run.status.success(),
        "depth experiment failed: {}",
        String::from_utf8_lossy(&depth_run.stderr)
    );
    let depth_report: Value = serde_json::from_slice(&depth_run.stdout).expect("depth report JSON");
    let depth_artifact = depth_report["artifact_path"]
        .as_str()
        .expect("depth report path");
    let depth_recomputed = Command::new(binary())
        .current_dir(&root)
        .args(["evidence", "recompute-depth-experiment", depth_artifact])
        .output()
        .expect("recompute depth experiment");
    assert!(
        depth_recomputed.status.success(),
        "depth recompute failed: {}",
        String::from_utf8_lossy(&depth_recomputed.stderr)
    );
    assert!(String::from_utf8_lossy(&depth_recomputed.stdout).contains("全部 1 个候选已复算一致"));

    let depth_bundle = root.join("depth-portable");
    let depth_packed = Command::new(binary())
        .current_dir(&root)
        .args([
            "evidence",
            "pack-depth-experiment",
            depth_artifact,
            "--output-dir",
        ])
        .arg(&depth_bundle)
        .output()
        .expect("pack depth experiment");
    assert!(
        depth_packed.status.success(),
        "depth package failed: {}",
        String::from_utf8_lossy(&depth_packed.stderr)
    );
    let moved_depth_bundle = moved_root.join("depth-portable");
    std::fs::rename(&depth_bundle, &moved_depth_bundle).expect("move depth package");
    std::fs::remove_file(&depth_path).expect("hide original depth input");
    std::fs::rename(root.join("depth-runs"), root.join("depth-runs-hidden"))
        .expect("hide original depth run artifacts");
    let depth_bundle_check = Command::new(binary())
        .current_dir(&foreign_cwd)
        .args(["evidence", "verify-depth-experiment"])
        .arg(&moved_depth_bundle)
        .arg("--recompute")
        .output()
        .expect("verify relocated depth experiment package");
    assert!(
        depth_bundle_check.status.success(),
        "relocated depth package failed: {}",
        String::from_utf8_lossy(&depth_bundle_check.stderr)
    );
    assert!(String::from_utf8_lossy(&depth_bundle_check.stdout).contains("整网格复算一致"));
    std::fs::remove_dir_all(root).expect("remove single-market recompute fixtures");
}

fn skipped_acceptance_replay(cwd: &Path, result: &str) -> FailureObservation {
    let acceptance = cwd.join("acceptance");
    std::fs::create_dir(&acceptance).expect("create acceptance input directory");
    std::fs::write(acceptance.join("result.json"), result).expect("copy skipped result");
    std::fs::write(acceptance.join("status.json"), "{}\n").expect("write status input");
    std::fs::write(acceptance.join("qianxing.runtime.acceptance.json"), "{}\n")
        .expect("write config input");
    observe_failure(
        &[
            "evidence",
            "pack-venue-acceptance",
            "acceptance",
            "--operator",
            "replay-test",
        ],
        cwd,
    )
}

#[test]
fn venue_acceptance_pack_rejects_the_recorded_skipped_testnet_window() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli tests are two levels below the workspace root");
    let result_path =
        repository.join("maturity/evidence/testnet/20260920T005856Z-dryrun/result.json");
    let result = std::fs::read_to_string(&result_path)
        .expect("read the recorded credential-free skipped testnet run");

    let first_cwd = temp_dir("venue-pack-skipped", 1);
    let first = skipped_acceptance_replay(&first_cwd, &result);
    std::fs::remove_dir_all(&first_cwd).expect("remove first skipped-window replay");

    let second_cwd = temp_dir("venue-pack-skipped", 2);
    let second = skipped_acceptance_replay(&second_cwd, &result);
    std::fs::remove_dir_all(&second_cwd).expect("remove second skipped-window replay");

    for observation in [&first, &second] {
        assert_eq!(observation.exit_code, 2);
        assert!(observation.stdout.trim().is_empty());
        assert!(
            observation.stderr.contains("只有 outcome=pass"),
            "skipped venue run was rejected for the wrong reason: {observation:?}"
        );
        assert!(
            !observation
                .artifacts
                .iter()
                .any(|path| path.ends_with("venue.evidence.json")),
            "skipped venue run produced L3 evidence: {observation:?}"
        );
    }
    assert_eq!(
        first, second,
        "skipped-window rejection changed across replays"
    );
}

fn successful_signature_replay(
    cwd: &Path,
) -> (super::Observation, super::Observation, super::Observation) {
    use qx_spec::{
        CapabilityLevel, EvidenceBundle, EvidenceClaim, EvidenceEnvironment, EvidenceWindow,
        VenueEvidencePayload,
    };

    let bundle = EvidenceBundle::venue(VenueEvidencePayload {
        claim: EvidenceClaim {
            summary: "固定测试窗口签名合同".into(),
            capability_level: CapabilityLevel::L3,
        },
        venue_id: "test-venue".into(),
        window: EvidenceWindow {
            start_ms: 1,
            end_ms: 2,
        },
        environment: EvidenceEnvironment {
            host: "local-test".into(),
            runtime_version: "test".into(),
            network: "testnet".into(),
        },
        redacted_config_digest: "a".repeat(64),
        logs: vec!["acceptance.log".into()],
        fact_count: 1,
        result_digest: "b".repeat(64),
        operator: "replay-test".into(),
    });
    let bundle_json = serde_json::to_string_pretty(&bundle).expect("serialize venue evidence");
    std::fs::write(cwd.join("venue.json"), bundle_json).expect("write venue evidence");
    let seed = "11".repeat(32);
    std::fs::write(cwd.join("seed.hex"), &seed).expect("write test-only signing seed");

    let signed = super::observe_in(
        cwd,
        &[
            "evidence",
            "sign",
            "venue.json",
            "--signing-key-file",
            "seed.hex",
            "--signature-file",
            "signature.json",
        ],
    );
    assert_eq!(signed.exit_code, 0, "evidence sign failed: {signed:?}");
    let sign_result: serde_json::Value =
        serde_json::from_str(&signed.stdout).expect("sign emits a JSON result");
    assert_eq!(sign_result["operation"], "signed");
    assert_eq!(sign_result["signature_algorithm"], "Ed25519");
    assert_eq!(sign_result["generation"], "venue");
    assert_eq!(sign_result["capability_level"], "L3");
    assert!(sign_result["public_key_hex"].as_str().is_some());
    assert!(
        !signed.stdout.contains(&seed),
        "signing seed leaked to stdout"
    );

    let public_key = sign_result["public_key_hex"]
        .as_str()
        .expect("sign result carries public key");
    std::fs::write(cwd.join("trusted.pub"), public_key).expect("write trusted test public key");
    let verified = super::observe_in(
        cwd,
        &[
            "evidence",
            "verify",
            "venue.json",
            "--signature-file",
            "signature.json",
            "--trusted-key-file",
            "trusted.pub",
        ],
    );
    assert_eq!(
        verified.exit_code, 0,
        "evidence verify failed: {verified:?}"
    );
    let verify_result: serde_json::Value =
        serde_json::from_str(&verified.stdout).expect("verify emits a JSON result");
    assert_eq!(verify_result["operation"], "signature-verified");
    assert_eq!(verify_result["venue_id"], "test-venue");
    assert_eq!(verify_result["capability_level"], "L3");

    let mut tampered: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(cwd.join("venue.json")).expect("read signed bundle"),
    )
    .expect("parse signed bundle");
    tampered["generation"]["venue"]["operator"] = "tampered-operator".into();
    std::fs::write(
        cwd.join("venue.json"),
        serde_json::to_vec_pretty(&tampered).expect("serialize tampered bundle"),
    )
    .expect("write tampered bundle");
    let tamper_rejected = super::observe_in(
        cwd,
        &[
            "evidence",
            "verify",
            "venue.json",
            "--signature-file",
            "signature.json",
            "--trusted-key-file",
            "trusted.pub",
        ],
    );
    assert_eq!(
        tamper_rejected.exit_code, 2,
        "verify accepted a modified signed bundle: {tamper_rejected:?}"
    );
    assert!(
        tamper_rejected.stdout.trim().is_empty(),
        "failed verification emitted success output"
    );
    (signed, verified, tamper_rejected)
}

#[test]
fn venue_evidence_sign_and_verify_replay_the_same_trusted_key_contract() {
    let first_cwd = temp_dir("evidence-signature", 1);
    let first = successful_signature_replay(&first_cwd);
    std::fs::remove_dir_all(&first_cwd).expect("remove first signature replay");

    let second_cwd = temp_dir("evidence-signature", 2);
    let second = successful_signature_replay(&second_cwd);
    std::fs::remove_dir_all(&second_cwd).expect("remove second signature replay");

    assert_eq!(
        first, second,
        "venue signature flow changed across clean replays"
    );
    assert!(first.0.artifacts.contains(&"signature.json".to_string()));
    assert!(first.1.artifacts.contains(&"trusted.pub".to_string()));
}
