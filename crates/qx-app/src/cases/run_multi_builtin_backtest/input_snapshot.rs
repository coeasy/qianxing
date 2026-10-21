use crate::{AppError, AppErrorCategory};
use qx_datastruct::BarFrame;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) struct MultiBuiltinScratch {
    pub(super) path: PathBuf,
    pub(super) serial: usize,
}

impl MultiBuiltinScratch {
    pub(super) fn create() -> Result<Self, AppError> {
        static SERIAL: AtomicUsize = AtomicUsize::new(0);
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "qx-app manifest 必须位于 workspace 根目录下",
                )
            })?
            .join("target/qx-app-multibuiltin")
            .join(format!("{}-{serial}", std::process::id()));
        std::fs::create_dir_all(&root).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("创建 qx-app 多腿临时产物目录失败: {error}"),
            )
        })?;
        Ok(Self { path: root, serial })
    }

    pub(super) fn write_frame(&self, name: &str, frame: &BarFrame) -> Result<PathBuf, AppError> {
        let path = self.path.join(name);
        frame.validate().map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("多腿回测输入 BarFrame 无法序列化: {error:?}"),
            )
        })?;
        std::fs::write(&path, frame.to_json()).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("写入多腿回测输入快照失败 {}: {error}", path.display()),
            )
        })?;
        Ok(path)
    }
}

impl Drop for MultiBuiltinScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
