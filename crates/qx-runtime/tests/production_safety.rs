use qx_runtime::{RuntimeConfig, WorkerRole};

#[test]
fn production_execution_requires_frozen_instrument_spec_even_for_paper_venue() {
    let mut config: RuntimeConfig = serde_json::from_str(include_str!(
        "../../../deploy/qianxing.runtime.production.example.json"
    ))
    .expect("production example must parse");

    let execution = config
        .workers
        .iter_mut()
        .find(|worker| worker.role == WorkerRole::Execution)
        .expect("production example must contain an execution worker");
    execution.venue_id = Some("paper".into());
    execution.instrument_spec_path = None;
    execution.credential_env = None;

    let error = config
        .validate()
        .expect_err("production execution without a frozen spec must fail closed");
    assert!(
        error.contains("instrument_spec_path"),
        "unexpected validation error: {error}"
    );
}
