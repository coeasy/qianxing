use super::*;

#[derive(Debug, PartialEq, Eq)]
struct FailureObservation {
    exit_code: i32,
    stdout: String,
    stderr: String,
    artifacts: Vec<String>,
}

fn observe_invalid_input(label: &str, args: &[&str], input_name: &str) -> FailureObservation {
    let cwd = temp_dir(label, 1);
    std::fs::write(cwd.join(input_name), "{").expect("write malformed local config");
    let output = Command::new(binary())
        .current_dir(&cwd)
        .env_remove("QX_DEPLOY_DIR")
        .env_remove("QX_PYTHON")
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run qx-cli {args:?}: {error}"));
    let cwd_text = cwd.to_string_lossy();
    let normalize = |text: &[u8]| {
        String::from_utf8_lossy(text)
            .replace(cwd_text.as_ref(), "<workdir>")
            .replace(&cwd_text.replace('\\', "/"), "<workdir>")
            .replace(&cwd_text.replace('/', "\\"), "<workdir>")
    };
    let artifacts = artifact_files(&cwd);
    let observation = FailureObservation {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: normalize(&output.stdout),
        stderr: normalize(&output.stderr),
        artifacts,
    };
    let _ = std::fs::remove_dir_all(cwd);
    observation
}

fn observe_in_isolated_dir(label: &str, args: &[&str]) -> FailureObservation {
    let cwd = temp_dir(label, 1);
    let output = Command::new(binary())
        .current_dir(&cwd)
        .env_remove("QX_DEPLOY_DIR")
        .env_remove("QX_PYTHON")
        .env_remove("QX_POSTGRES_DSN")
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run qx-cli {args:?}: {error}"));
    let observation = FailureObservation {
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        artifacts: artifact_files(&cwd),
    };
    let _ = std::fs::remove_dir_all(cwd);
    observation
}

#[test]
fn optional_storage_and_messaging_commands_reject_before_external_io() {
    let cases: [(&str, &[&str], &str); 6] = [
        (
            "storage-adopt-postgres",
            &["storage-adopt-postgres", "--schema", "qx_test"],
            "需要使用 --features postgres 构建 qx-cli",
        ),
        (
            "outbox-relay",
            &[
                "outbox-relay",
                "target/outbox",
                "nats://127.0.0.1:4222",
                "qx",
            ],
            "需要使用 --features nats 构建 qx-cli",
        ),
        (
            "outbox-relay-postgres",
            &[
                "outbox-relay-postgres",
                "deploy/runtime.json",
                "nats://127.0.0.1:4222",
                "qx",
            ],
            "需要使用 --features 'nats postgres' 构建 qx-cli",
        ),
        (
            "outbox-relay-worker",
            &["outbox-relay-worker", "deploy/runtime.json", "relay-1"],
            "需要使用 --features nats 构建 qx-cli",
        ),
        (
            "event-consumer-worker",
            &["event-consumer-worker", "deploy/runtime.json", "consumer-1"],
            "需要使用 --features nats 构建 qx-cli",
        ),
        (
            "consumer-dlq-replay",
            &[
                "consumer-dlq-replay",
                "deploy/runtime.json",
                "group-1",
                "event-1",
            ],
            "需要使用 --features nats 构建 qx-cli",
        ),
    ];

    for (label, args, expected_diagnostic) in cases {
        // The feature-off contract applies only when that backend is not compiled in. Feature
        // matrix builds validate the enabled path through its own runtime/config contracts; they
        // must not keep expecting the disabled-feature message after enabling the backend.
        if (label == "storage-adopt-postgres" && cfg!(feature = "postgres"))
            || (matches!(
                label,
                "outbox-relay"
                    | "outbox-relay-worker"
                    | "event-consumer-worker"
                    | "consumer-dlq-replay"
            ) && cfg!(feature = "nats"))
            || (label == "outbox-relay-postgres"
                && cfg!(all(feature = "nats", feature = "postgres")))
        {
            continue;
        }
        let first = observe_in_isolated_dir(label, args);
        let second = observe_in_isolated_dir(label, args);
        assert_eq!(
            first.exit_code, 2,
            "{label} must fail closed without its backend feature: {first:?}"
        );
        assert!(
            first.stderr.contains(expected_diagnostic),
            "{label} must identify its required feature: {first:?}"
        );
        assert!(
            first.artifacts.is_empty(),
            "{label} must reject before creating local artifacts: {first:?}"
        );
        assert_eq!(
            first, second,
            "{label} rejection changed across clean process replays"
        );
    }
}

#[test]
fn binance_public_probe_rejects_unsupported_network_before_external_io() {
    let run = |replay| {
        let cwd = temp_dir("binance-public-probe-invalid-network", replay);
        let output = Command::new(binary())
            .current_dir(&cwd)
            .args(["binance-public-probe", "unsupported-network"])
            .output()
            .expect("run invalid Binance public-probe network");
        let observation = FailureObservation {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            artifacts: artifact_files(&cwd),
        };
        let _ = std::fs::remove_dir_all(cwd);
        observation
    };

    let first = run(1);
    let second = run(2);
    assert_eq!(first.exit_code, 2, "invalid network must exit 2: {first:?}");
    assert!(
        first.stderr.contains("network 必须是 testnet 或 mainnet"),
        "failure must identify the accepted network names: {first:?}"
    );
    assert!(
        first.artifacts.is_empty(),
        "failure must not write files: {first:?}"
    );
    assert_eq!(
        first, second,
        "invalid network failure changed across replays"
    );
}

#[test]
fn ccxt_snapshot_commands_reject_malformed_config_before_external_io() {
    let cases: [(&str, &[&str], &str); 2] = [
        (
            "ccxt-fetch-ohlcv",
            &[
                "ccxt-fetch-ohlcv",
                "bad-ccxt.json",
                "BTC/USDT",
                "0",
                "1",
                "snapshot.json",
            ],
            "OHLCV",
        ),
        (
            "ccxt-market-spec",
            &[
                "ccxt-market-spec",
                "bad-ccxt.json",
                "BTC/USDT",
                "market.json",
            ],
            "market spec",
        ),
    ];
    for (label, args, diagnostic) in cases {
        let first = observe_invalid_input(label, args, "bad-ccxt.json");
        let second = observe_invalid_input(label, args, "bad-ccxt.json");
        assert_eq!(first.exit_code, 2, "{label} must fail closed: {first:?}");
        assert!(
            first.stderr.contains("解析 CCXT worker 配置失败"),
            "{label} should identify its malformed local config: {first:?}"
        );
        assert!(first.stderr.contains(diagnostic));
        assert_eq!(
            first, second,
            "{label} changed across clean process replays"
        );
        assert_eq!(
            first.artifacts,
            ["bad-ccxt.json"],
            "{label} wrote output before validating its config"
        );
    }
}

#[test]
fn binance_private_probe_rejects_malformed_runtime_before_network_io() {
    let args = [
        "binance-private-probe",
        "bad-runtime.json",
        "binance-execution-main",
    ];
    let first = observe_invalid_input("binance-private-probe", &args, "bad-runtime.json");
    let second = observe_invalid_input("binance-private-probe", &args, "bad-runtime.json");
    assert_eq!(
        first.exit_code, 2,
        "private probe must fail closed: {first:?}"
    );
    assert!(
        first.stderr.contains("运行时配置 JSON 无效"),
        "private probe should report its malformed local config: {first:?}"
    );
    assert_eq!(
        first, second,
        "private probe changed across clean process replays"
    );
    assert_eq!(first.artifacts, ["bad-runtime.json"]);
}

#[test]
fn ccxt_submit_order_rejects_malformed_runtime_before_command_or_network_io() {
    let args = [
        "ccxt-submit-order",
        "bad-runtime.json",
        "ccxt-execution-main",
        "ccxt.json",
        "command.json",
    ];
    let first = observe_invalid_input("ccxt-submit-order", &args, "bad-runtime.json");
    let second = observe_invalid_input("ccxt-submit-order", &args, "bad-runtime.json");
    assert_eq!(
        first.exit_code, 2,
        "CCXT submit must fail closed: {first:?}"
    );
    assert!(
        first.stderr.contains("运行时配置 JSON 无效"),
        "CCXT submit should reject its malformed runtime before reading command data: {first:?}"
    );
    assert_eq!(
        first, second,
        "CCXT submit changed across clean process replays"
    );
    assert_eq!(first.artifacts, ["bad-runtime.json"]);
}

#[test]
fn console_and_supervise_reject_malformed_runtime_before_starting_services() {
    let cases: [(&str, &[&str]); 2] = [
        ("console", &["console", "bad-runtime.json"]),
        ("supervise", &["supervise", "bad-runtime.json"]),
    ];
    for (label, args) in cases {
        let first = observe_invalid_input(label, args, "bad-runtime.json");
        let second = observe_invalid_input(label, args, "bad-runtime.json");
        assert_eq!(first.exit_code, 2, "{label} must fail closed: {first:?}");
        assert!(
            first.stderr.contains("运行时配置 JSON 无效"),
            "{label} should reject malformed runtime before service startup: {first:?}"
        );
        assert_eq!(first, second, "{label} changed across clean replays");
        assert_eq!(first.artifacts, ["bad-runtime.json"]);
    }
}

#[test]
fn runtime_worker_and_service_commands_reject_invalid_config_before_side_effects() {
    let cases: [(&str, &[&str]); 9] = [
        ("serve", &["serve", "bad-runtime.json"]),
        (
            "scheduler-worker",
            &["scheduler-worker", "bad-runtime.json", "worker-id"],
        ),
        (
            "strategy-worker",
            &["strategy-worker", "bad-runtime.json", "worker-id"],
        ),
        (
            "paper-worker",
            &["paper-worker", "bad-runtime.json", "worker-id"],
        ),
        (
            "binance-worker",
            &["binance-worker", "bad-runtime.json", "worker-id"],
        ),
        (
            "ccxt-worker",
            &["ccxt-worker", "bad-runtime.json", "worker-id", "ccxt.json"],
        ),
        ("reconcile", &["reconcile", "bad-runtime.json"]),
        (
            "binance-submit-order",
            &[
                "binance-submit-order",
                "bad-runtime.json",
                "worker-id",
                "command.json",
            ],
        ),
        (
            "paper-submit-order",
            &["paper-submit-order", "bad-runtime.json", "command.json"],
        ),
    ];
    for (label, args) in cases {
        let first = observe_invalid_input(label, args, "bad-runtime.json");
        let second = observe_invalid_input(label, args, "bad-runtime.json");
        assert_eq!(first.exit_code, 2, "{label} must fail closed: {first:?}");
        assert!(
            first.stderr.contains("运行时配置 JSON 无效"),
            "{label} should reject malformed runtime before side effects: {first:?}"
        );
        assert_eq!(first, second, "{label} changed across clean replays");
        assert_eq!(first.artifacts, ["bad-runtime.json"]);
    }
}
