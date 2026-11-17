//! Explicit runtime config explanation, fingerprinting, and lock materialization commands.

use super::*;

pub(crate) fn run_config_explain(path: &Path, as_json: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    if as_json {
        // RuntimeConfig 只包含凭据引用（环境变量名/投影文件路径），不包含
        // secret 内容；输出可直接交给脚本或配置审计工具继续处理。
        println!("{}", config.to_json()?);
        return Ok(());
    }
    let strategy_count = config.strategies.len().max(1);
    println!("[配置 · 有效] path={}", path.display());
    println!(
        "  schema={} environment={} profile={:?}",
        config.schema_version, config.environment, config.profile
    );
    println!(
        "  storage={:?} data_dir={} api={} transport={:?}",
        config.storage.backend,
        // 与 doctor 同口径：data_dir 有两个落点（运行态按当前目录、产物按 runtime.json 同级）。
        effective_storage_root(path, &config.storage.data_dir).display(),
        config.api.bind,
        config.api.transport
    );
    println!(
        "  workers={} enabled={} strategies={} fingerprint={} locked={}",
        config.workers.len(),
        config
            .workers
            .iter()
            .filter(|worker| worker.enabled)
            .count(),
        strategy_count,
        config.fingerprint()?,
        config.config_fingerprint.is_some()
    );
    for worker in config.workers.iter().filter(|worker| worker.enabled) {
        println!(
            "  worker id={} role={:?} account={} venue={}",
            worker.id,
            worker.role,
            worker.account_id.as_deref().unwrap_or("-"),
            worker.venue_id.as_deref().unwrap_or("-")
        );
    }
    println!("[配置 · 安全] 未读取密钥内容，仅检查引用名称和文件路径");
    Ok(())
}

pub(crate) fn run_config_fingerprint(path: &Path) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    println!(
        "[配置 · Fingerprint] path={} fingerprint={}",
        path.display(),
        config.fingerprint()?
    );
    Ok(())
}

pub(crate) fn run_config_lock(input: &Path, output: &Path, force: bool) -> Result<(), String> {
    let config = read_runtime_config(input)?;
    let fingerprint = config.fingerprint()?;
    if output.exists() && !force {
        return Err(format!(
            "目标发布配置已存在: {}；如确认覆盖，请显式添加 --force",
            output.display()
        ));
    }
    let mut locked = config;
    locked.config_fingerprint = Some(fingerprint.clone());
    let payload = locked.to_json()?;
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建发布配置目录失败 {}: {error}", parent.display()))?;
    }
    std::fs::write(output, payload)
        .map_err(|error| format!("写入发布配置失败 {}: {error}", output.display()))?;
    let verified = read_runtime_config(output)?;
    verified.verify_fingerprint()?;
    println!(
        "[配置 · Lock] input={} output={} fingerprint={} locked=true",
        input.display(),
        output.display(),
        fingerprint
    );
    Ok(())
}
