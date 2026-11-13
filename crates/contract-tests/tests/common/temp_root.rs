use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TEMP_ROOT: AtomicU64 = AtomicU64::new(0);

pub(crate) fn test_temp_root(label: &str) -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp");
    std::fs::create_dir_all(&base).expect("create contract-test temp root");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after Unix epoch")
        .as_nanos();
    let serial = NEXT_TEMP_ROOT.fetch_add(1, Ordering::Relaxed);
    base.join(format!(
        "qianxing-contract-{label}-{}-{nanos}-{serial}",
        std::process::id()
    ))
}
