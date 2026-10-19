//! Convert unwinds at long-lived CLI service boundaries into a stable, non-retryable diagnostic.

pub(crate) fn catch_panic<T>(
    boundary: &str,
    run: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        Ok(result) => result,
        Err(payload) => Err(panic_diagnostic(boundary, payload.as_ref())),
    }
}

pub(crate) fn panic_diagnostic(boundary: &str, payload: &(dyn std::any::Any + Send)) -> String {
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
