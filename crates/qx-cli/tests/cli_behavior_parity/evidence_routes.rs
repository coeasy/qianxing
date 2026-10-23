use super::{artifact_digests, artifact_files, binary, temp_dir};
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
    let cases: [(&str, &[&str]); 16] = [
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
