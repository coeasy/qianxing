use super::*;
use qx_core::{EventKind, Priority};

fn root() -> PathBuf {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-storage is two levels below workspace root");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after UNIX epoch")
        .as_nanos();
    let root = workspace.join("target").join(format!(
        "qx-storage-segment-sync-interruption-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create isolated store root");
    root
}

fn append(log: &mut EventLog, ts: u64) {
    let seq = log.alloc_seq();
    log.append(Event::new(seq, ts, Priority::POST, EventKind::Settle));
}

#[test]
fn synced_active_append_before_manifest_commit_recovers_and_retries() {
    let root = root();
    let store = SegmentedEventLogStore::new(&root, 4).unwrap();
    let mut committed = EventLog::new();
    append(&mut committed, 1);
    store.write("run", &committed).unwrap();
    let committed_manifest = std::fs::read(root.join("run.manifest.json")).unwrap();

    let mut intended = committed.clone();
    append(&mut intended, 2);
    INTERRUPT_AFTER_SEGMENT_SYNC.with(|interrupt| interrupt.set(true));
    let error = store.write("run", &intended).unwrap_err();
    assert!(matches!(
        error,
        StorageError::Io(message) if message == "injected interruption after segment sync"
    ));
    assert_eq!(
        std::fs::read(root.join("run.manifest.json")).unwrap(),
        committed_manifest,
        "the old manifest remains the commit point"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("segments/run-0000000000000000.jsonl")).unwrap(),
        intended
            .events()
            .iter()
            .map(|event| format!("{}\n", serde_json::to_string(event).unwrap()))
            .collect::<String>(),
        "the real append path synced the complete uncommitted suffix before interruption"
    );

    let restarted = SegmentedEventLogStore::new(&root, 4).unwrap();
    assert_eq!(restarted.read("run").unwrap().digest(), committed.digest());
    restarted.write("run", &intended).unwrap();
    assert_eq!(restarted.read("run").unwrap().digest(), intended.digest());

    let _ = std::fs::remove_dir_all(root);
}
