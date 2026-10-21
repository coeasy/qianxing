//! Unified `run` usage text derived from the generated command manifest.

/// Read the clap-derived manifest without maintaining a second command list.
pub(crate) fn run_entry_points() -> Result<Vec<String>, String> {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("cli_command_manifest.json"))
            .map_err(|error| format!("解析生成的 CLI 命令清单失败: {error}"))?;
    let commands = manifest
        .get("commands")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "生成的 CLI 命令清单缺少 commands 数组".to_string())?;
    commands
        .iter()
        .filter(|route| route["parent"] == "run")
        .map(|route| {
            route["name"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "run 命令清单项缺少字符串 name".to_string())
        })
        .collect()
}

pub(crate) fn run_usage(phrase: &str) -> String {
    match run_entry_points() {
        Ok(entries) => format!("run {phrase}；可用入口：{}", entries.join("、")),
        Err(error) => format!("run {phrase}；入口清单无效：{error}"),
    }
}
