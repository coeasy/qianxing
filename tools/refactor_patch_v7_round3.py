from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    if new in text:
        return
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected exactly one guarded match, got {count}")
    p.write_text(text.replace(old, new, 1))


runtime = "crates/qx-runtime/src/lib.rs"
replace_once(
    runtime,
    '''                Ok(Err(error)) => {
                    let _ = context.mark(ServiceStatus::Failed, error.clone(), None);
                    Err(format!("worker {thread_id} failed"))
                }
''',
    '''                Ok(Err(error)) => {
                    let _ = context.mark(ServiceStatus::Failed, error.clone(), None);
                    Err(format!("worker {thread_id} failed: {error}"))
                }
''',
)

replace_once(
    runtime,
    '''    #[test]
    fn runtime_config_round_trips_and_rejects_production_plaintext() {
''',
    '''    #[test]
    fn supervisor_propagates_worker_failure_cause() {
        let supervisor = RuntimeSupervisor::new(config()).unwrap();
        let handle = supervisor
            .spawn_worker("market", |_| Err("sentinel worker failure".into()))
            .unwrap();
        let error = handle.join().unwrap().unwrap_err();
        assert!(error.contains("sentinel worker failure"));
    }

    #[test]
    fn runtime_config_round_trips_and_rejects_production_plaintext() {
''',
)
