#![cfg(test)]

use super::*;
use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn framed_response_and_worker_error_payloads_are_accepted() {
    for (response, expected) in [
        (
            StrategyFrame::response(7, br#"{"ok":true}"#.to_vec()),
            r#"{"ok":true}"#,
        ),
        (
            StrategyFrame::error(7, br#"{"ok":false}"#.to_vec()),
            r#"{"ok":false}"#,
        ),
    ] {
        assert_eq!(
            response_payload(StrategyWorkerResponse::Frame(response), 7, "test"),
            Ok(expected.into())
        );
    }
}

#[test]
fn framed_response_rejects_a_stale_sequence() {
    let error = response_payload(
        StrategyWorkerResponse::Frame(StrategyFrame::response(6, b"{}".to_vec())),
        7,
        "test",
    )
    .unwrap_err();
    assert!(error.contains("expected=7 actual=6"), "{error}");
}

#[test]
fn framed_response_rejects_request_frames_and_non_utf8_payloads() {
    let wrong_kind = response_payload(
        StrategyWorkerResponse::Frame(StrategyFrame::request(7, b"{}".to_vec())),
        7,
        "test",
    )
    .unwrap_err();
    assert!(wrong_kind.contains("非响应分帧"), "{wrong_kind}");

    let invalid_utf8 = response_payload(
        StrategyWorkerResponse::Frame(StrategyFrame::response(7, vec![0xff])),
        7,
        "test",
    )
    .unwrap_err();
    assert!(invalid_utf8.contains("不是 UTF-8"), "{invalid_utf8}");
}

#[test]
fn physical_pipe_rejects_bad_magic_and_crc_then_reaps_the_worker() {
    let mut bad_crc = StrategyFrame::response(1, b"{}".to_vec())
        .encode(DEFAULT_MAX_FRAME_BYTES)
        .unwrap();
    bad_crc[20] ^= 1;
    for (bytes, expected_error) in [
        (&[0_u8; 24][..], "magic 不匹配"),
        (&bad_crc, "CRC32 不匹配"),
    ] {
        let (executable, args) = malformed_frame_worker(bytes);
        let env = std::collections::BTreeMap::new();
        let mut worker = StrategyWorkerProcess::spawn(StrategyWorkerSpawnConfig {
            executable: &executable,
            args: &args,
            env: &env,
            timeout_ms: 1_000,
            label: "framed-test",
            diagnostic_program: &executable,
            transport: StrategyTransport::FramedJson,
            ring_config: SharedRingConfig::default(),
            ring_directory: None,
        })
        .unwrap();

        let error = worker.request("{}".into(), None).unwrap_err();
        assert!(error.contains("分帧响应失败"), "{error}");
        assert!(error.contains(expected_error), "{error}");
        assert!(error.contains("进程未退出"), "{error}");

        let started = Instant::now();
        drop(worker);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "损坏帧后的 worker 未在回收预算内停止"
        );
    }
}

#[test]
fn spawn_failure_reports_worker_identity_and_cleans_shared_rings() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after UNIX_EPOCH")
        .as_nanos();
    let ring_directory = std::env::current_dir()
        .expect("workspace directory")
        .join("target")
        .join(format!(
            "qx-orchestrator-spawn-failure-{}-{unique}",
            std::process::id()
        ));
    std::fs::create_dir_all(&ring_directory).expect("isolated ring directory");
    let missing_executable = ring_directory.join("missing-strategy-worker");
    let missing_executable = missing_executable.to_string_lossy().into_owned();
    let env = std::collections::BTreeMap::new();
    let error = match StrategyWorkerProcess::spawn(StrategyWorkerSpawnConfig {
        executable: &missing_executable,
        args: &[],
        env: &env,
        timeout_ms: 100,
        label: "spawn-failure-test",
        diagnostic_program: "configured-python",
        transport: StrategyTransport::SharedMemoryJson,
        ring_config: SharedRingConfig::default(),
        ring_directory: Some(&ring_directory),
    }) {
        Ok(_) => panic!("missing worker executable unexpectedly spawned"),
        Err(error) => error,
    };

    assert!(
        error.contains("启动 spawn-failure-test worker 失败"),
        "{error}"
    );
    assert!(error.contains("程序=configured-python"), "{error}");
    let remaining = std::fs::read_dir(&ring_directory)
        .expect("ring directory remains available")
        .collect::<Result<Vec<_>, _>>()
        .expect("read ring directory");
    assert!(
        remaining.is_empty(),
        "spawn failure leaked ring files: {remaining:?}"
    );
    std::fs::remove_dir(&ring_directory).expect("remove empty isolated ring directory");
}

#[test]
fn worker_exit_before_first_request_preserves_exit_code_and_stderr() {
    let (executable, args) = exiting_worker();
    let env = std::collections::BTreeMap::new();
    let mut worker = StrategyWorkerProcess::spawn(StrategyWorkerSpawnConfig {
        executable: &executable,
        args: &args,
        env: &env,
        timeout_ms: 1_000,
        label: "early-exit-test",
        diagnostic_program: "configured-strategy-worker",
        transport: StrategyTransport::FramedJson,
        ring_config: SharedRingConfig::default(),
        ring_directory: None,
    })
    .expect("worker process should spawn before exiting");

    let exit_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if worker
            .child
            .as_mut()
            .expect("spawned child is present")
            .try_wait()
            .expect("child exit status is readable")
            .is_some()
        {
            break;
        }
        assert!(
            Instant::now() < exit_deadline,
            "worker did not exit promptly"
        );
        thread::sleep(Duration::from_millis(1));
    }

    let stderr_deadline = Instant::now() + Duration::from_secs(2);
    while !worker
        .diagnostics()
        .contains("strategy died before request")
        && Instant::now() < stderr_deadline
    {
        thread::sleep(Duration::from_millis(1));
    }
    let error = worker.request("{}".into(), None).unwrap_err();
    assert!(error.contains("early-exit-test"), "{error}");
    assert!(error.contains("退出码 7"), "{error}");
    assert!(error.contains("strategy died before request"), "{error}");
}

#[test]
fn e2b_host_provider_key_uses_separate_allowlisted_environment_channel() {
    let (executable, args) = print_e2b_key_worker();
    let strategy_env = std::collections::BTreeMap::new();
    let provider_env = std::collections::BTreeMap::from([(
        "E2B_API_KEY".to_string(),
        "host-only-test-key".to_string(),
    )]);
    let mut pipes = crate::spawn_strategy_process_with_host_provider_env(
        &executable,
        &args,
        &strategy_env,
        &provider_env,
        StrategyTransport::Jsonl,
    )
    .expect("trusted E2B bridge can receive its host SDK credential");
    drop(pipes.stdin.take());
    let mut stdout = String::new();
    pipes
        .stdout
        .take()
        .expect("stdout pipe")
        .read_to_string(&mut stdout)
        .expect("read provider environment output");
    assert!(pipes
        .child
        .wait()
        .expect("provider test process exits")
        .success());
    assert_eq!(stdout.trim(), "host-only-test-key");

    let untrusted_env = std::collections::BTreeMap::from([(
        "E2B_API_KEY".to_string(),
        "must-not-pass".to_string(),
    )]);
    let error = crate::spawn_strategy_process_with_host_provider_env(
        &executable,
        &args,
        &untrusted_env,
        &std::collections::BTreeMap::new(),
        StrategyTransport::Jsonl,
    )
    .err()
    .expect("ordinary strategy environment cannot carry provider credentials");
    assert!(error.contains("可能包含交易凭证"), "{error}");

    let unsupported_provider_env = std::collections::BTreeMap::from([(
        "ARBITRARY_SECRET".to_string(),
        "must-not-pass".to_string(),
    )]);
    let error = crate::spawn_strategy_process_with_host_provider_env(
        &executable,
        &args,
        &strategy_env,
        &unsupported_provider_env,
        StrategyTransport::Jsonl,
    )
    .err()
    .expect("provider environment only admits E2B_API_KEY");
    assert!(
        error.contains("unsupported host provider credential"),
        "{error}"
    );
}

fn print_e2b_key_worker() -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        let executable = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string())
            + "\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
        (
            executable,
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                "[Console]::Out.WriteLine($env:E2B_API_KEY)".into(),
            ],
        )
    }
    #[cfg(unix)]
    {
        (
            "/bin/sh".into(),
            vec!["-c".into(), "printf '%s\\n' \"$E2B_API_KEY\"".into()],
        )
    }
}

fn exiting_worker() -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        let executable = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string())
            + "\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
        (
            executable,
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                "[Console]::Error.WriteLine('strategy died before request'); exit 7".into(),
            ],
        )
    }
    #[cfg(unix)]
    {
        (
            "/bin/sh".into(),
            vec![
                "-c".into(),
                "printf '%s\\n' 'strategy died before request' >&2; exit 7".into(),
            ],
        )
    }
}

fn malformed_frame_worker(bytes: &[u8]) -> (String, Vec<String>) {
    #[cfg(windows)]
    {
        let executable = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string())
            + "\\System32\\WindowsPowerShell\\v1.0\\powershell.exe";
        let bytes = bytes
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let script = format!(
            "$bytes=[byte[]]({bytes}); Start-Sleep -Milliseconds 100; $out=[Console]::OpenStandardOutput(); $out.Write($bytes,0,$bytes.Length); $out.Flush(); Start-Sleep -Milliseconds 2000"
        );
        (
            executable,
            vec!["-NoProfile".into(), "-Command".into(), script],
        )
    }
    #[cfg(unix)]
    {
        let bytes = bytes
            .iter()
            .map(|byte| format!("\\{byte:03o}"))
            .collect::<String>();
        (
            "/bin/sh".into(),
            vec!["-c".into(), format!("sleep 0.1; printf '{bytes}'; sleep 2")],
        )
    }
}
