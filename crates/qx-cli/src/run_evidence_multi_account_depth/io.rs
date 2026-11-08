//! Atomic file publication and Evidence bundle IO for multi-account depth packages.

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

pub(super) fn reserve_staging_directory(
    parent: &Path,
    target: &Path,
) -> Result<StagingDirectory, String> {
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("multi-account-evidence");
    for _ in 0..32 {
        let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{name}.staging-{}-{sequence}", std::process::id()));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(StagingDirectory(candidate)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "创建多账户证据临时目录失败 {}: {error}",
                    candidate.display()
                ))
            }
        }
    }
    Err("无法保留唯一的多账户证据临时目录".into())
}

pub(super) fn write_new(path: &Path, bytes: &[u8], label: &str) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("创建{label}失败 {}: {error}", path.display()))?;
    if let Err(error) = file.write_all(bytes) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(format!("写入{label}失败 {}: {error}", path.display()));
    }
    Ok(())
}

pub(super) fn verify_digest(path: &Path, expected: &str, label: &str) -> Result<(), String> {
    let actual = qx_strategy::file_digest::sha256_file_hex(path)?;
    if actual != expected {
        return Err(format!(
            "{label} SHA-256 不匹配: 声明={expected} 实际={actual}"
        ));
    }
    Ok(())
}
