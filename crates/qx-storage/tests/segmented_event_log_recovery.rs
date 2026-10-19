use qx_core::{Event, EventKind, EventLog, Priority};
use qx_storage::SegmentedEventLogStore;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

fn root(label: &str) -> PathBuf {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-storage tests are two levels below the workspace root");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after UNIX epoch")
        .as_nanos();
    let path = workspace.join("target").join(format!(
        "qx-storage-segment-recovery-{label}-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).expect("create isolated segmented log directory");
    path
}

fn append(log: &mut EventLog, ts: u64) {
    let seq = log.alloc_seq();
    log.append(Event::new(seq, ts, Priority::POST, EventKind::Settle));
}

fn segment_bytes(events: &[Event]) -> String {
    events
        .iter()
        .map(|event| format!("{}\n", serde_json::to_string(event).unwrap()))
        .collect()
}

fn segment(root: &Path, index: usize) -> PathBuf {
    root.join("segments").join(format!("run-{index:016}.jsonl"))
}

#[test]
fn interrupted_active_tail_is_read_as_committed_prefix_and_retry_completes() {
    let root = root("active-tail");
    let first_store = SegmentedEventLogStore::new(&root, 2).unwrap();
    let mut committed = EventLog::new();
    append(&mut committed, 1);
    first_store.write("run", &committed).unwrap();

    let mut intended = committed.clone();
    append(&mut intended, 2);
    // Simulate a crash after the atomic segment replacement but before manifest publication.
    std::fs::write(segment(&root, 0), segment_bytes(intended.events())).unwrap();

    let restarted = SegmentedEventLogStore::new(&root, 2).unwrap();
    let visible = restarted.read("run").unwrap();
    assert_eq!(visible.digest(), committed.digest());
    assert_eq!(visible.len(), 1);
    restarted.write("run", &intended).unwrap();
    assert_eq!(restarted.read("run").unwrap().digest(), intended.digest());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn partial_active_tail_append_is_ignored_until_retry_finishes_the_same_bytes() {
    let root = root("partial-active-tail");
    let store = SegmentedEventLogStore::new(&root, 2).unwrap();
    let mut committed = EventLog::new();
    append(&mut committed, 1);
    store.write("run", &committed).unwrap();

    let mut intended = committed.clone();
    append(&mut intended, 2);
    let committed_bytes = segment_bytes(committed.events());
    let intended_bytes = segment_bytes(intended.events());
    let suffix = &intended_bytes.as_bytes()[committed_bytes.len()..];
    let partial_len = suffix.len() / 2;
    std::fs::OpenOptions::new()
        .append(true)
        .open(segment(&root, 0))
        .unwrap()
        .write_all(&suffix[..partial_len])
        .unwrap();

    let restarted = SegmentedEventLogStore::new(&root, 2).unwrap();
    assert_eq!(restarted.read("run").unwrap().digest(), committed.digest());
    restarted.write("run", &intended).unwrap();
    assert_eq!(
        std::fs::read_to_string(segment(&root, 0)).unwrap(),
        intended_bytes
    );
    assert_eq!(restarted.read("run").unwrap().digest(), intended.digest());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn committed_manifest_ignores_an_uncommitted_non_utf8_suffix() {
    let root = root("non-utf8-active-tail");
    let store = SegmentedEventLogStore::new(&root, 2).unwrap();
    let mut committed = EventLog::new();
    append(&mut committed, 1);
    store.write("run", &committed).unwrap();

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(segment(&root, 0))
        .unwrap();
    file.write_all(&[0xff]).unwrap();
    file.sync_all().unwrap();

    let restarted = SegmentedEventLogStore::new(&root, 2).unwrap();
    assert_eq!(restarted.read("run").unwrap().digest(), committed.digest());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn writer_preserves_compact_jsonl_bytes_across_incremental_tail_append() {
    let root = root("jsonl-bytes");
    let store = SegmentedEventLogStore::new(&root, 2).unwrap();
    let mut first = EventLog::new();
    append(&mut first, 1);
    append(&mut first, 2);
    store.write("run", &first).unwrap();

    let mut extended = first.clone();
    append(&mut extended, 3);
    store.write("run", &extended).unwrap();

    assert_eq!(
        std::fs::read_to_string(segment(&root, 0)).unwrap(),
        segment_bytes(&extended.events()[..2])
    );
    assert_eq!(
        std::fs::read_to_string(segment(&root, 1)).unwrap(),
        segment_bytes(&extended.events()[2..])
    );
    assert_eq!(store.read("run").unwrap().digest(), extended.digest());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn committed_segment_corruption_is_rejected_without_mutating_the_manifest() {
    let root = root("committed-corruption");
    let store = SegmentedEventLogStore::new(&root, 2).unwrap();
    let mut committed = EventLog::new();
    append(&mut committed, 1);
    append(&mut committed, 2);
    store.write("run", &committed).unwrap();
    let manifest_path = root.join("run.manifest.json");
    let manifest_before = std::fs::read(&manifest_path).unwrap();

    let path = segment(&root, 0);
    let original = std::fs::read_to_string(&path).unwrap();
    let changed = original.replacen("\"ts\":1", "\"ts\":9", 1);
    assert_ne!(changed, original, "fixture must alter a committed event");
    std::fs::write(path, changed).unwrap();

    let reopened = SegmentedEventLogStore::new(&root, 2).unwrap();
    assert!(reopened.read("run").is_err());
    assert_eq!(std::fs::read(manifest_path).unwrap(), manifest_before);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn malformed_committed_segment_is_rejected_as_corruption() {
    let root = root("malformed-committed");
    let store = SegmentedEventLogStore::new(&root, 2).unwrap();
    let mut committed = EventLog::new();
    append(&mut committed, 1);
    store.write("run", &committed).unwrap();
    std::fs::write(segment(&root, 0), "{\"truncated\":\n").unwrap();

    let reopened = SegmentedEventLogStore::new(&root, 2).unwrap();
    assert!(reopened.read("run").is_err());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn interrupted_new_segment_is_ignored_until_retry_publishes_manifest() {
    let root = root("orphan-segment");
    let first_store = SegmentedEventLogStore::new(&root, 2).unwrap();
    let mut committed = EventLog::new();
    append(&mut committed, 1);
    append(&mut committed, 2);
    first_store.write("run", &committed).unwrap();

    let mut intended = committed.clone();
    append(&mut intended, 3);
    // Simulate a crash after writing the next segment while the old manifest is still committed.
    std::fs::write(segment(&root, 1), segment_bytes(&intended.events()[2..])).unwrap();

    let restarted = SegmentedEventLogStore::new(&root, 2).unwrap();
    let visible = restarted.read("run").unwrap();
    assert_eq!(visible.digest(), committed.digest());
    assert_eq!(visible.len(), 2);
    restarted.write("run", &intended).unwrap();
    assert_eq!(restarted.read("run").unwrap().digest(), intended.digest());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn retry_rejects_a_divergent_uncommitted_tail() {
    let root = root("divergent-tail");
    let store = SegmentedEventLogStore::new(&root, 2).unwrap();
    let mut committed = EventLog::new();
    append(&mut committed, 1);
    store.write("run", &committed).unwrap();

    let mut divergent = committed.clone();
    append(&mut divergent, 3);
    std::fs::write(segment(&root, 0), segment_bytes(divergent.events())).unwrap();
    let mut intended = committed.clone();
    append(&mut intended, 2);

    let restarted = SegmentedEventLogStore::new(&root, 2).unwrap();
    assert_eq!(restarted.read("run").unwrap().digest(), committed.digest());
    assert!(restarted.write("run", &intended).is_err());
    assert_eq!(restarted.read("run").unwrap().digest(), committed.digest());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn legacy_v1_manifest_reads_and_upgrades_to_v2_on_append() {
    let root = root("v1-upgrade");
    let mut committed = EventLog::new();
    append(&mut committed, 1);
    let segment_name = "run-0000000000000000.jsonl";
    std::fs::create_dir_all(root.join("segments")).unwrap();
    std::fs::write(segment(&root, 0), segment_bytes(committed.events())).unwrap();
    std::fs::write(
        root.join("run.manifest.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "name": "run",
            "event_count": committed.len(),
            "next_seq": committed.next_seq(),
            "digest": committed.digest(),
            "segments": [segment_name],
        }))
        .unwrap(),
    )
    .unwrap();

    let store = SegmentedEventLogStore::new(&root, 2).unwrap();
    assert_eq!(store.read("run").unwrap().digest(), committed.digest());
    let mut extended = committed.clone();
    append(&mut extended, 2);
    store.write("run", &extended).unwrap();
    let upgraded: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("run.manifest.json")).unwrap()).unwrap();
    assert_eq!(upgraded["schema_version"], 2);
    assert_eq!(store.read("run").unwrap().digest(), extended.digest());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn independent_store_instances_serialize_divergent_extensions() {
    let root = root("cross-instance");
    let seed_store = SegmentedEventLogStore::new(&root, 2).unwrap();
    let mut base = EventLog::new();
    append(&mut base, 1);
    seed_store.write("run", &base).unwrap();

    let mut left = base.clone();
    append(&mut left, 2);
    let mut right = base;
    append(&mut right, 3);
    let barrier = Arc::new(Barrier::new(2));
    let run_append = |log: EventLog| {
        let store = SegmentedEventLogStore::new(&root, 2).unwrap();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store.write("run", &log)
        })
    };
    let left = run_append(left);
    let right = run_append(right);
    let outcomes = [left.join().unwrap(), right.join().unwrap()];
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Err(qx_storage::StorageError::NonAppendOnly(_))))
            .count(),
        1
    );
    assert_eq!(
        SegmentedEventLogStore::new(&root, 2)
            .unwrap()
            .read("run")
            .unwrap()
            .len(),
        2
    );

    let _ = std::fs::remove_dir_all(root);
}
