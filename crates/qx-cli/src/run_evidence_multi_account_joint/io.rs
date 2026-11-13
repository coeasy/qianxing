//! Atomic file publication and Evidence bundle IO for joint account packages.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use qx_spec::{EvidenceBundle, FoundationDocument};

pub(super) static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) struct StagingDirectory(pub(super) PathBuf);

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn read_run_bundle(path: &Path) -> Result<EvidenceBundle, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取账户 EvidenceBundle 失败 {}: {error}", path.display()))?;
    EvidenceBundle::from_json(&payload)
}

pub(super) fn verify_digest(path: &Path, expected: &str, label: &str) -> Result<(), String> {
    let actual = qx_strategy::file_digest::sha256_file_hex(path)?;
    if actual != expected {
        return Err(format!(
            "{label} SHA-256 不匹配: expected={expected} actual={actual}"
        ));
    }
    Ok(())
}

pub(super) fn write_new(path: &Path, bytes: &[u8], label: &str) -> Result<(), String> {
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("创建 {label} 失败 {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("写入 {label} 失败 {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("刷新 {label} 失败 {}: {error}", path.display()))
}

pub(super) fn reserve_staging_directory(
    parent: &Path,
    target: &Path,
) -> Result<StagingDirectory, String> {
    for _ in 0..32 {
        let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{}.joint-evidence-staging-{}-{sequence}",
            target
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("package"),
            std::process::id()
        ));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(StagingDirectory(candidate)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("创建联合证据暂存目录失败: {error}")),
        }
    }
    Err("无法预留唯一联合证据暂存目录".into())
}
