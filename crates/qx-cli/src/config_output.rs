//! `config validate` / `fingerprint` / `lock` 三种命令的输出实现（人读与 `--json` 机读）。
//!
//! 从 `cli.rs` 的分派正文与 `config_commands.rs` 拆出（V11 E4）：这三条命令过去声明了
//! `--json` 却只用它抑制横幅，旗标本身不产出任何 JSON。拆出来之后"旗标有没有被真的读到"
//! 由 `tools/check_architecture.py` 的 `config_json_surface_check` 逐命令判定。
//! 有效配置的解读（`config explain`）与状态查询仍留在 `config_commands.rs`。

use super::*;

/// `config validate`：只判配置引用的本地文件与凭据名是否可用，不读密钥内容。
/// 机读分支把警告与失败原样装进信封后仍然返回 `Err`，让退出码 2 与 JSON 结论同向。
pub(crate) fn run_config_validate(path: &Path, as_json: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let (failures, warnings) = validate_runtime_references(path, &config);
    if as_json {
        let report = serde_json::json!({
            "schema_version": 1,
            "command": "config validate",
            "runtime_path": path.display().to_string(),
            "ok": failures.is_empty(),
            "warnings": &warnings,
            "failures": &failures,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("编码 config validate JSON 失败: {error}"))?
        );
    } else {
        for warning in warnings {
            println!("[WARN] {warning}");
        }
        for failure in &failures {
            eprintln!("[FAIL] {failure}");
        }
        if failures.is_empty() {
            println!("[PASS] config validate 通过: {}", path.display());
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("配置引用校验失败，共 {} 项", failures.len()))
    }
}

pub(crate) fn run_config_fingerprint(path: &Path, as_json: bool) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let fingerprint = config.fingerprint()?;
    if as_json {
        let report = serde_json::json!({
            "schema_version": 1,
            "command": "config fingerprint",
            "runtime_path": path.display().to_string(),
            "fingerprint": fingerprint,
            // 文件里已写下的发布指纹：`null` 才是"未锁定"，不能省成看不见的差别。
            "config_fingerprint": &config.config_fingerprint,
            "locked": config.config_fingerprint.is_some(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("编码 config fingerprint JSON 失败: {error}"))?
        );
        return Ok(());
    }
    println!(
        "[配置 · Fingerprint] path={} fingerprint={}",
        path.display(),
        fingerprint
    );
    Ok(())
}

pub(crate) fn run_config_lock(
    input: &Path,
    output: &Path,
    force: bool,
    as_json: bool,
) -> Result<(), String> {
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
    if as_json {
        let report = serde_json::json!({
            "schema_version": 1,
            "command": "config lock",
            "input_path": input.display().to_string(),
            "output_path": output.display().to_string(),
            "fingerprint": fingerprint,
            "locked": true,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("编码 config lock JSON 失败: {error}"))?
        );
        return Ok(());
    }
    println!(
        "[配置 · Lock] input={} output={} fingerprint={} locked=true",
        input.display(),
        output.display(),
        fingerprint
    );
    Ok(())
}
