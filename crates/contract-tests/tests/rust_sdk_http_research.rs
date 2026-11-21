//! Real TCP server and Rust SDK: data validation, backtest, verification and async lifecycle.
//! This test runs in its own test process because API roots are configured through environment variables.
use qx_api::{ApiService, ApiState};
use qx_sdk_http::{
    BacktestDatasetInput, BacktestOutcome, BacktestSpec, BuiltinStrategyKind, BuiltinStrategySpec,
    CallerCapability, Client, DatasetSpec, HttpRemoteTransport, RemoteApiClient, RemoteApiError,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

fn drop_response_proxy(port: u16) -> (u16, thread::JoinHandle<()>) {
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_port = proxy.local_addr().unwrap().port();
    let proxy_worker = thread::spawn(move || {
        let (mut incoming, _) = proxy.accept().unwrap();
        incoming
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        let expected = loop {
            let count = incoming.read(&mut buffer).unwrap();
            assert_ne!(count, 0, "request ended before headers");
            request.extend_from_slice(&buffer[..count]);
            assert!(request.len() <= 1_048_576);
            if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&request[..end]).unwrap();
                let size: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                break end + 4 + size;
            }
        };
        while request.len() < expected {
            let count = incoming.read(&mut buffer).unwrap();
            assert_ne!(count, 0, "request ended before its body");
            request.extend_from_slice(&buffer[..count]);
            assert!(request.len() <= 1_048_576);
        }
        let mut upstream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        upstream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        upstream.write_all(&request[..expected]).unwrap();
        let mut response = Vec::new();
        upstream.read_to_end(&mut response).unwrap();
        assert!(
            response.starts_with(b"HTTP/1.1 2"),
            "{}",
            String::from_utf8_lossy(&response)
        );
        // `incoming` closes without forwarding the successful response.
    });
    (proxy_port, proxy_worker)
}

#[test]
fn local_and_remote_sdk_share_research_results_and_server_controls_artifact_paths() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let test_root = std::env::var_os("QX_RESEARCH_TEST_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repo.join("target"));
    let root = test_root.join(format!("remote-research-{}-{nonce}", std::process::id()));
    let data_root = root.join("data");
    let artifact_root = root.join("artifacts");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::create_dir_all(&artifact_root).unwrap();
    let bars = data_root.join("bars.json");
    std::fs::copy(repo.join("deploy/qianxing.bar-frame.example.json"), &bars).unwrap();
    std::env::set_var("QX_API_DATA_ROOT", &data_root);
    std::env::set_var("QX_API_ARTIFACT_ROOT", &artifact_root);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let service = ApiService::new(ApiState::default()).with_local_paper_control();
    let api_state = service.state();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_worker = Arc::clone(&stop);
    let worker = thread::spawn(move || {
        service
            .serve(listener, || 1, || stop_worker.load(Ordering::Acquire))
            .unwrap();
    });
    let transport =
        HttpRemoteTransport::loopback("127.0.0.1", port, Duration::from_secs(5)).unwrap();
    let remote = RemoteApiClient::new(transport);
    let verdict = remote
        .validate_dataset(&DatasetSpec::new("example", "bars.json"))
        .unwrap();
    assert!(verdict.usable);
    assert_eq!(verdict.rows, 70);
    assert!(matches!(
        remote.validate_dataset(&DatasetSpec::new("escape", "../outside.json")),
        Err(RemoteApiError::Http { status: 400, .. })
    ));
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "remote-sma");
    strategy.fast_window = 2;
    strategy.slow_window = 3;
    let spec = BacktestSpec::new(
        BacktestDatasetInput {
            run_id: "sync-research".into(),
            instrument: "BTCUSDT.BINANCE".into(),
            bars_path: "bars.json".into(),
        },
        "USDT",
        100_000 * 1_000_000_000,
        7,
        "ignored-client-output",
        strategy,
    );
    let outcome = remote.run_backtest(&spec).unwrap();
    assert_eq!(outcome.fills, 1);
    assert!(Path::new(&outcome.artifacts.summary).starts_with(&artifact_root));
    let verified = remote.verify_run(&outcome).unwrap();
    assert!(verified.verified, "{:?}", verified.mismatches);
    assert_eq!(verified.result_hash, outcome.result_hash);
    let mut local_spec = spec.clone();
    local_spec.bars_path = bars.to_string_lossy().into_owned();
    local_spec.output_dir = root.join("local").to_string_lossy().into_owned();
    let local = Client::new(CallerCapability::Research, "local")
        .run_backtest(&local_spec)
        .unwrap();
    assert_eq!(
        (local.result_hash, local.data_fingerprint, local.fills),
        (
            outcome.result_hash.clone(),
            outcome.data_fingerprint.clone(),
            outcome.fills
        )
    );

    let mut async_spec = spec.clone();
    async_spec.run_id = "async-research".into();
    let started = remote.start_backtest(&async_spec).unwrap();
    assert_eq!(started["run_id"], "async-research");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        let status = remote.run_status("async-research").unwrap();
        match status["status"].as_str().unwrap() {
            "succeeded" => break status,
            "failed" | "cancelled" => panic!("unexpected terminal status: {status}"),
            _ => assert!(Instant::now() < deadline, "async run did not finish"),
        }
        thread::sleep(Duration::from_millis(10));
    };
    let async_outcome: BacktestOutcome = serde_json::from_value(status["result"].clone()).unwrap();
    assert!(remote.verify_run(&async_outcome).unwrap().verified);
    assert!(remote.get_run_events("async-research", 0).is_ok());
    let cancelled_after_completion = remote.cancel_run("async-research").unwrap();
    assert_eq!(cancelled_after_completion["status"], "succeeded");
    assert!(matches!(
        remote.run_status("unknown-run"),
        Err(RemoteApiError::Http { status: 404, .. })
    ));
    assert!(remote.cancel_run("../outside").is_err());

    // Forward a real start request to the API but deliberately drop its response.
    // The SDK must expose uncertainty and the original ID, so callers can query
    // the accepted run instead of blindly starting it a second time.
    let (proxy_port, proxy_worker) = drop_response_proxy(port);
    let lost_transport =
        HttpRemoteTransport::loopback("127.0.0.1", proxy_port, Duration::from_secs(5)).unwrap();
    let lost_client = RemoteApiClient::new(lost_transport);
    let mut lost_spec = spec.clone();
    lost_spec.run_id = "lost-response".into();
    let error = lost_client.start_backtest(&lost_spec).unwrap_err();
    assert!(matches!(error, RemoteApiError::OutcomeUnknown {
        recovery_id: Some(id), ..
    } if id == "lost-response"));
    proxy_worker.join().unwrap();
    assert_eq!(
        remote.run_status("lost-response").unwrap()["run_id"],
        "lost-response"
    );
    let (python_proxy_port, python_proxy) = drop_response_proxy(port);
    let (python_paper_proxy_port, python_paper_proxy) = drop_response_proxy(port);
    let python = std::env::var_os("QX_PYTHON").unwrap_or_else(|| {
        if cfg!(windows) {
            "python".into()
        } else {
            "python3".into()
        }
    });
    let python_output = Command::new(python)
        .arg(repo.join("crates/contract-tests/tests/fixtures/python_remote_research.py"))
        .arg(format!("http://127.0.0.1:{port}"))
        .arg(serde_json::to_string(&spec).unwrap())
        .arg(format!("http://127.0.0.1:{python_proxy_port}"))
        .arg(format!("http://127.0.0.1:{python_paper_proxy_port}"))
        .env("PYTHONPATH", repo.join("python"))
        .output()
        .expect("launch Python remote SDK consumer");
    assert!(
        python_output.status.success(),
        "Python remote SDK failed: {}\n{}",
        String::from_utf8_lossy(&python_output.stdout),
        String::from_utf8_lossy(&python_output.stderr)
    );
    python_proxy.join().unwrap();
    python_paper_proxy.join().unwrap();
    let audit_state = api_state.lock().unwrap();
    let audit = audit_state.control.audit();
    for request_id in ["python-paper-command", "python-paper-lost-response"] {
        assert_eq!(
            audit
                .iter()
                .filter(|record| record.request_id == request_id)
                .count(),
            1,
            "Paper command must be accepted exactly once: {request_id}"
        );
    }
    drop(audit_state);
    let remote_audit = remote.get_command_audit().unwrap();
    for request_id in ["python-paper-command", "python-paper-lost-response"] {
        assert_eq!(
            remote_audit
                .iter()
                .filter(|record| record.request_id == request_id)
                .count(),
            1,
            "Rust SDK audit read must expose exactly one recovered Paper command: {request_id}"
        );
    }
    let recovered = remote
        .get_command_audit_by_request_id("python-paper-lost-response")
        .unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].request_id, "python-paper-lost-response");
    stop.store(true, Ordering::Release);
    worker.join().unwrap();
}
