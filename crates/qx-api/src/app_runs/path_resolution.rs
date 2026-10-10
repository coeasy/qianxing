//! Restrict application API input paths to the configured data root.

use qx_app::{AppError, AppErrorCategory};
use std::path::{Component, Path};

/// Absolute paths are accepted only beneath the configured root; traversal and
/// symlinks resolving outside the root are refused.
pub(crate) fn resolve_input_path(path: &mut String, data_root: &Path) -> Result<(), AppError> {
    std::fs::create_dir_all(data_root).map_err(|error| {
        AppError::from_io(
            &format!("创建 API 数据根目录 {}", data_root.display()),
            &error,
        )
    })?;
    let root = std::fs::canonicalize(data_root).map_err(|error| {
        AppError::from_io(
            &format!("解析 API 数据根目录 {}", data_root.display()),
            &error,
        )
    })?;
    let input = Path::new(path);
    let lexical_root = if data_root.is_absolute() {
        data_root.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| AppError::from_io("解析 API 工作目录", &error))?
            .join(data_root)
    };
    let relative = if input.is_absolute() {
        input.strip_prefix(&lexical_root).map_err(|_| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                "HTTP 文件路径必须位于服务配置的根目录内",
            )
        })?
    } else {
        input
    };
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "HTTP 文件路径必须是配置根目录下的相对路径",
        ));
    }
    let candidate = root.join(relative);
    if candidate.exists() {
        let resolved = std::fs::canonicalize(&candidate).map_err(|error| {
            AppError::from_io(
                &format!("解析 API 数据文件 {}", candidate.display()),
                &error,
            )
        })?;
        if !resolved.starts_with(&root) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "HTTP 文件不能通过符号链接越出服务配置的根目录",
            ));
        }
        *path = resolved.to_string_lossy().into_owned();
    } else {
        *path = candidate.to_string_lossy().into_owned();
    }
    Ok(())
}
