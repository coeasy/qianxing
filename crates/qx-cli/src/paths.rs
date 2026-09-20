//! 路径解析：运行时配置目录相对路径、凭据引用与规格文件的统一解析规则。
//!
//! 所有相对路径都以 runtime.json 所在目录为基准，绝对路径原样保留。

use super::*;

pub(crate) fn runtime_path(root: &Path, configured: &str) -> std::path::PathBuf {
    let path = Path::new(configured);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

pub(crate) fn repository_deploy_path(file_name: &str) -> PathBuf {
    let source_tree_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join(file_name);
    if source_tree_path.exists() {
        source_tree_path
    } else {
        PathBuf::from("deploy").join(file_name)
    }
}

pub(crate) fn default_runtime_path() -> PathBuf {
    repository_deploy_path("qianxing.runtime.example.json")
}

pub(crate) fn resolve_ccxt_config_path(runtime_path: &Path, configured: &str) -> String {
    resolve_runtime_relative_path(runtime_path, configured)
        .to_string_lossy()
        .into_owned()
}

fn is_explicit_absolute_path(configured: &str) -> bool {
    let bytes = configured.as_bytes();
    Path::new(configured).is_absolute()
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
        || configured.starts_with("\\\\")
}

pub(crate) fn resolve_runtime_relative_path(runtime_path: &Path, configured: &str) -> PathBuf {
    if is_explicit_absolute_path(configured) {
        PathBuf::from(configured)
    } else {
        let parent = runtime_path.parent().unwrap_or_else(|| Path::new("."));
        let configured_path = Path::new(configured);

        // Runtime JSON 通常位于 deploy/ 下。兼容两种常见写法：
        // `qianxing.foo.json` 与 `deploy/qianxing.foo.json`；后者在
        // deploy/runtime.json 下直接 join 会错误地变成 deploy/deploy/。
        // 只在配置首段恰好等于运行时目录名时剥离该冗余段，避免改变
        // 其它目录布局的语义。
        let mut components = configured_path.components();
        let first = components.next();
        let parent_name = parent.file_name().and_then(|value| value.to_str());
        if let (Some(std::path::Component::Normal(first)), Some(parent_name)) = (first, parent_name)
        {
            if first == std::ffi::OsStr::new(parent_name) {
                let remainder = components.as_path();
                return parent.join(remainder);
            }
        }
        parent.join(configured_path)
    }
}

pub(crate) fn resolve_runtime_asset_path(
    runtime_config_path: &Path,
    storage_root: &Path,
    configured: &str,
) -> PathBuf {
    let config_candidate = resolve_runtime_relative_path(runtime_config_path, configured);
    if Path::new(configured).is_absolute() || config_candidate.exists() {
        return config_candidate;
    }
    let storage_candidate = runtime_path(storage_root, configured);
    if storage_candidate.exists() {
        return storage_candidate;
    }
    config_candidate
}

pub(crate) fn resolve_worker_runtime_paths(worker: &mut WorkerConfig, runtime_path: &Path) {
    if let Some(files) = worker.credential_files.as_mut() {
        files.api_key = resolve_runtime_relative_path(runtime_path, &files.api_key)
            .to_string_lossy()
            .into_owned();
        files.secret = resolve_runtime_relative_path(runtime_path, &files.secret)
            .to_string_lossy()
            .into_owned();
    }
}
