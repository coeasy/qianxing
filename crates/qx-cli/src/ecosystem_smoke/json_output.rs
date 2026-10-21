//! Fallible JSON stdout writers shared by smoke and evidence commands.

pub(crate) fn print_json_report(report: &serde_json::Value) -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, report)
        .map_err(|error| format!("写入 JSON 报告到 stdout 失败: {error}"))?;
    std::io::Write::write_all(&mut stdout, b"\n")
        .map_err(|error| format!("写入 JSON 报告换行到 stdout 失败: {error}"))
}

pub(crate) fn print_machine_json(report: &serde_json::Value) -> Result<(), String> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, report)
        .map_err(|error| format!("写入机器 JSON 到 stdout 失败: {error}"))?;
    std::io::Write::write_all(&mut stdout, b"\n")
        .map_err(|error| format!("写入机器 JSON 换行到 stdout 失败: {error}"))
}
