//! 路径解析：运行时相对路径、策略产物路径与显式绝对路径判定。

use super::*;

pub(crate) fn resolve_ccxt_config_path(runtime_path: &Path, configured: &str) -> String {
    resolve_runtime_relative_path(runtime_path, configured)
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn is_explicit_absolute_path(configured: &str) -> bool {
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

/// `storage.data_dir` 实际被打开的落点。
///
/// 这个字段有两种口径：可写运行态（控制面、队列、EventLog、outbox、metrics）按进程
/// 当前目录打开，回测产物（`runs/`、`datasets.manifest.json`）按 runtime.json 同级目录
/// 写入。相对配置值在两个口径下会指向不同目录，所以诊断命令必须报告真正在用的那个，
/// 而不是任选一种折算：以进程目录口径为主，只有它不存在而配置目录口径存在时才报告后者。
/// 两处都有状态时由调用方并列提示——那意味着同一份配置换了启动目录，账本和回测已分家。
pub(crate) fn effective_storage_root(runtime_path: &Path, configured: &str) -> PathBuf {
    let process_root = Path::new(configured);
    if process_root.exists() {
        return process_root.to_path_buf();
    }
    let config_root = resolve_runtime_relative_path(runtime_path, configured);
    if config_root.exists() {
        return config_root;
    }
    process_root.to_path_buf()
}

/// doctor 的 `storage.data_dir` 检查：可用性与提示按**两个**落点口径共同判断。
///
/// 只按其中一种折算会让 doctor 指向一个没有写入者使用的目录——真实账本已存在却被提示成
/// "将在首次运行时创建"，两处都还没创建时又会把可创建的落点误判成父目录不可用。报告给出
/// 真正在被使用的落点（[`effective_storage_root`]），并在两处都已落盘时提示配置分家。
pub(crate) fn check_storage_data_dir(
    runtime_path: &Path,
    configured: &str,
    checks: &mut Vec<serde_json::Value>,
    warnings: &mut Vec<String>,
    failures: &mut Vec<String>,
) {
    let process_root = Path::new(configured);
    let config_relative_root = resolve_runtime_relative_path(runtime_path, configured);
    let candidates = [process_root, config_relative_root.as_path()];
    let data_dir = effective_storage_root(runtime_path, configured);
    let canonical = candidates
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .collect::<Vec<_>>();
    if canonical.len() == 2 && canonical[0] != canonical[1] {
        let message = format!(
            "storage.data_dir 有两个已存在的落点: {}（进程目录口径，运行态写在这里）与 {}（配置目录口径，回测产物写在这里）；请固定启动目录或改用绝对路径，否则同一配置的账本与回测互不可见",
            process_root.display(),
            config_relative_root.display()
        );
        checks.push(serde_json::json!({
            "name": "storage.data_dir.split",
            "status": "warn",
            "message": message
        }));
        warnings.push(message);
    }
    if let Some(blocked) = candidates
        .iter()
        .find(|root| root.exists() && !root.is_dir())
    {
        let message = format!("storage.data_dir 不是目录: {}", blocked.display());
        checks.push(serde_json::json!({
            "name": "storage.data_dir",
            "status": "fail",
            "message": message
        }));
        failures.push(message);
    } else if candidates.iter().any(|root| root.is_dir()) {
        checks.push(serde_json::json!({
            "name": "storage.data_dir",
            "status": "pass",
            "message": data_dir.display().to_string()
        }));
    } else if candidates
        .iter()
        .any(|root| root.parent().is_some_and(|parent| parent.is_dir()))
    {
        let message = format!(
            "storage.data_dir 尚不存在，将在首次运行时创建: {}",
            data_dir.display()
        );
        checks.push(serde_json::json!({
            "name": "storage.data_dir",
            "status": "warn",
            "message": message
        }));
        warnings.push(message);
    } else {
        let message = format!("storage.data_dir 的父目录不存在: {}", data_dir.display());
        checks.push(serde_json::json!({
            "name": "storage.data_dir",
            "status": "fail",
            "message": message
        }));
        failures.push(message);
    }
}

pub(crate) fn resolve_strategy_runtime_paths(
    strategy: &mut StrategyRuntimeConfig,
    runtime_path: &Path,
) {
    if let Some(configured) = strategy.target_snapshot_path.as_deref() {
        strategy.target_snapshot_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(configured) = strategy.research_snapshot_path.as_deref() {
        strategy.research_snapshot_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(configured) = strategy.dataset_bundle_path.as_deref() {
        strategy.dataset_bundle_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    for configured in strategy.dataset_component_paths.values_mut() {
        *configured = resolve_runtime_relative_path(runtime_path, configured)
            .to_string_lossy()
            .into_owned();
    }
    if let Some(configured) = strategy.bars_snapshot_path.as_deref() {
        strategy.bars_snapshot_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(configured) = strategy.ashare_rules_path.as_deref() {
        strategy.ashare_rules_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(configured) = strategy.ashare_actions_path.as_deref() {
        strategy.ashare_actions_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(configured) = strategy.ashare_calendar_path.as_deref() {
        strategy.ashare_calendar_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(configured) = strategy.builtin_reference_bars_snapshot_path.as_deref() {
        strategy.builtin_reference_bars_snapshot_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(executable) = strategy.external_executable.as_deref() {
        let path_like = executable.contains('/')
            || executable.contains('\\')
            || executable.starts_with('.')
            || Path::new(executable).is_absolute();
        if path_like {
            strategy.external_executable = Some(
                resolve_runtime_relative_path(runtime_path, executable)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    if let Some(module) = strategy.python_module.as_deref() {
        let path_like = module.ends_with(".py") || module.contains('/') || module.contains('\\');
        if path_like {
            strategy.python_module = Some(
                resolve_runtime_relative_path(runtime_path, module)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    if let Some(library) = strategy.c_abi_library.as_deref() {
        let path_like = library.contains('/')
            || library.contains('\\')
            || library.starts_with('.')
            || Path::new(library).is_absolute();
        if path_like {
            strategy.c_abi_library = Some(
                resolve_runtime_relative_path(runtime_path, library)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
}

pub(crate) fn verify_strategy_artifact(strategy: &StrategyRuntimeConfig) -> Result<(), String> {
    let Some(expected) = strategy.strategy_artifact_sha256.as_deref() else {
        return Ok(());
    };
    if let Some(reference) = strategy.external_executable.as_deref() {
        return qx_strategy::verify_file_sha256(reference, expected)
            .map_err(|error| format!("策略发布物校验失败 {}: {error}", reference));
    }
    let reference = strategy
        .python_module
        .as_deref()
        .ok_or_else(|| "strategy_artifact_sha256 缺少策略文件引用".to_string())?;
    // Python 可以配置 importable module name；Rust host 无法在不复制 Python
    // import 规则的情况下定位它，交由 Python worker 在 import 后按 __file__ 校验。
    // 显式文件路径仍在 spawn 前由 host 先校验，形成双重门禁。
    if !Path::new(reference).is_file()
        && !(reference.ends_with(".py")
            || reference.contains('/')
            || reference.contains('\\')
            || reference.starts_with('.')
            || Path::new(reference).is_absolute())
    {
        return Ok(());
    }
    qx_strategy::verify_file_sha256(reference, expected)
        .map_err(|error| format!("策略发布物校验失败 {}: {error}", reference))
}
