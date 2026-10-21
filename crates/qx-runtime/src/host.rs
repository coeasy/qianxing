//! Shared service-host lifecycle helpers.
//!
//! Facades map command or transport input to a configured role. This module owns the
//! bounded worker shutdown and panic-to-diagnostic boundary used while hosting that role.

use std::{thread, time::Duration};

use crate::RuntimeSupervisor;

/// Join a worker under the runtime shutdown budget, forwarding process termination to it.
pub fn join_worker_handle(
    supervisor: &RuntimeSupervisor,
    handle: std::thread::JoinHandle<Result<(), String>>,
    role: &str,
    worker_id: &str,
) -> Result<(), String> {
    let label = format!("{role} worker {worker_id}");
    crate::install_shutdown_signals();
    let started = std::time::Instant::now();
    match crate::wait_for_worker_finish(
        supervisor,
        || handle.is_finished(),
        crate::shutdown_signalled,
        || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        |millis| thread::sleep(Duration::from_millis(millis)),
    ) {
        crate::WorkerLadder::Finished => {}
        crate::WorkerLadder::StoppedWithinBudget { waited_ms } => {
            println!("[停机 · Shutdown] {label} 按停机请求退出 waited_ms={waited_ms}")
        }
        crate::WorkerLadder::StopTimedOut { waited_ms } => {
            return Err(format!(
                "{label} 收到停机请求后 {waited_ms}ms 仍未退出，超过 shutdown_timeout_ms"
            ));
        }
    }
    handle
        .join()
        .map_err(|error| format!("{label} panic: {error:?}"))?
}

/// Convert an unwind at a long-lived service boundary into a stable diagnostic.
pub fn catch_panic<T>(
    boundary: &str,
    run: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(result) => result,
        Err(payload) => Err(panic_diagnostic(boundary, payload.as_ref())),
    }
}

/// Format the non-retryable invariant diagnostic for a service boundary unwind.
pub fn panic_diagnostic(boundary: &str, payload: &(dyn std::any::Any + Send)) -> String {
    let message = payload
        .downcast_ref::<&str>()
        .map(|value| (*value).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string());
    format!(
        "category=INTERNAL_INVARIANT action=REPORT_BUG retry=NEVER safe_to_retry=false correlation_id={boundary}-{} message={message:?}",
        std::process::id()
    )
}

#[cfg(test)]
mod tests {
    use super::catch_panic;

    #[test]
    fn host_boundary_preserves_successful_results() {
        let result = catch_panic("worker-test", || Ok::<_, String>(42));
        assert_eq!(result, Ok(42));
    }

    #[test]
    fn host_boundary_maps_panics_to_non_retryable_diagnostics() {
        let result = catch_panic::<()>("worker-test", || panic!("worker exploded"));
        let error = result.expect_err("panic must not escape the hosted boundary");
        assert!(error.contains("category=INTERNAL_INVARIANT"), "{error}");
        assert!(error.contains("action=REPORT_BUG"), "{error}");
        assert!(error.contains("retry=NEVER"), "{error}");
        assert!(error.contains("correlation_id=worker-test-"), "{error}");
        assert!(error.contains("worker exploded"), "{error}");
    }
}
