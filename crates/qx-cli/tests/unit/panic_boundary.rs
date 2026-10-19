use super::*;
use std::process::Command;

#[test]
fn cli_process_boundary_catches_panics_and_emits_non_retryable_diagnostic() {
    let diagnostic = run_cli_with_panic_boundary(|| panic!("injected worker panic"))
        .expect_err("panic must not escape the CLI process boundary");

    for field in [
        "category=INTERNAL_INVARIANT",
        "action=REPORT_BUG",
        "retry=NEVER",
        "safe_to_retry=false",
        "correlation_id=cli-process-",
        "injected worker panic",
    ] {
        assert!(
            diagnostic.contains(field),
            "diagnostic missing {field}: {diagnostic}"
        );
    }
}

#[test]
fn worker_service_boundaries_catch_panics_with_the_same_non_retryable_contract() {
    let diagnostic = catch_panic("worker-entry", || -> Result<(), String> {
        panic!("injected worker panic")
    })
    .expect_err("worker entry panic must become a diagnostic");

    for field in [
        "category=INTERNAL_INVARIANT",
        "action=REPORT_BUG",
        "retry=NEVER",
        "safe_to_retry=false",
        "correlation_id=worker-entry-",
        "injected worker panic",
    ] {
        assert!(diagnostic.contains(field), "missing {field}: {diagnostic}");
    }
}

#[test]
fn worker_panic_child_process_exits_nonzero_and_keeps_the_panic_hook() {
    const INJECT: &str = "QX_TEST_INJECT_WORKER_PANIC";
    if std::env::var_os(INJECT).is_some() {
        let mut ledger = qx_core::Ledger::new();
        let id = ledger
            .deposit("panic-probe", "USD", Money::from_i64(100), 1)
            .expect("准备已落账事实");
        let entry = ledger
            .entries()
            .iter()
            .find(|entry| entry.id == id)
            .expect("已落账条目")
            .clone();
        let mut log = qx_core::EventLog::new();
        let sequence = log.alloc_seq();
        log.append(qx_core::Event::new(
            sequence,
            entry.ts,
            qx_core::Priority::APPLY,
            qx_core::EventKind::LedgerApplied { entry },
        ));
        let diagnostic = catch_panic("worker-entry", || -> Result<(), String> {
            panic!("injected worker panic")
        })
        .expect_err("injected child panic must become a diagnostic");
        eprintln!("[qx-cli · Panic] {diagnostic}");
        qx_core::ReplayVerifier::verify(log.events(), &ledger)
            .expect("panic 后既有 Ledger 事实仍能复核");
        eprintln!("ledger_verify=pass");
        std::process::exit(101);
    }

    let output = Command::new(std::env::current_exe().expect("测试进程路径"))
        .args([
            "--exact",
            "tests::panic_boundary::worker_panic_child_process_exits_nonzero_and_keeps_the_panic_hook",
            "--nocapture",
        ])
        .env(INJECT, "1")
        .output()
        .expect("启动 panic 子进程");
    assert_eq!(output.status.code(), Some(101), "子进程必须非零退出");
    let stderr = String::from_utf8_lossy(&output.stderr);
    for required in [
        "injected worker panic",
        "category=INTERNAL_INVARIANT",
        "action=REPORT_BUG",
        "retry=NEVER",
        "safe_to_retry=false",
        "correlation_id=worker-entry-",
        "ledger_verify=pass",
    ] {
        assert!(
            stderr.contains(required),
            "子进程 stderr 缺少 {required}: {stderr}"
        );
    }
}
