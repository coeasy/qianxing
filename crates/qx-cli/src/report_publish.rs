use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static STAGE_SERIAL: AtomicUsize = AtomicUsize::new(0);

/// Collect generated report outputs and publish them through one rollback-capable sink.
#[derive(Default)]
pub(crate) struct ReportOutputBundle {
    outputs: Vec<(PathBuf, Vec<u8>)>,
    staged_paths: Vec<PathBuf>,
}

impl ReportOutputBundle {
    pub(crate) fn add_bytes(&mut self, target: &Path, bytes: Vec<u8>) {
        self.outputs.push((target.to_path_buf(), bytes));
    }

    pub(crate) fn stage_path(&mut self, target: &Path) -> Result<PathBuf, String> {
        let parent = target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建报告暂存目录失败 {}: {error}", parent.display()))?;
        let stem = target
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| format!("报告输出文件名无效: {}", target.display()))?;
        let extension = target
            .extension()
            .and_then(|extension| extension.to_str())
            .ok_or_else(|| format!("报告输出缺少文件扩展名: {}", target.display()))?;
        loop {
            let serial = STAGE_SERIAL.fetch_add(1, Ordering::Relaxed);
            let staged = parent.join(format!(
                ".{stem}.qx-stage-{}-{serial}.{extension}",
                std::process::id()
            ));
            if !staged.exists() {
                self.staged_paths.push(staged.clone());
                return Ok(staged);
            }
        }
    }

    pub(crate) fn add_staged(&mut self, target: &Path, staged: &Path) -> Result<(), String> {
        let bytes = std::fs::read(staged)
            .map_err(|error| format!("读取暂存报告失败 {}: {error}", staged.display()))?;
        self.add_bytes(target, bytes);
        Ok(())
    }

    pub(crate) fn extend(&mut self, outputs: Vec<(PathBuf, Vec<u8>)>) {
        self.outputs.extend(outputs);
    }

    pub(crate) fn commit(&self) -> Result<(), String> {
        if self.outputs.is_empty() {
            return Ok(());
        }
        qx_app::artifact_store::sink::publish_bundle(&self.outputs)
            .map_err(|error| format!("发布报告产物 bundle 失败: {error}"))
    }
}

impl Drop for ReportOutputBundle {
    fn drop(&mut self) {
        for path in &self.staged_paths {
            let _ = std::fs::remove_file(path);
        }
    }
}
