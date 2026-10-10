//! `run <入口>` 的参数读法：至多一个可选的配置文件路径，加上逐入口认账的 `--json`。
//!
//! `config_commands.rs` 里那五条单配置文件入口此前各写一遍"挑第一个非旗标参数，剩下的
//! 丢掉"，于是 `run doctor a.json b.json` 与 `run doctor --verbose` 都会被收下并以 0 退出，
//! 而同一个 match 的 `backtest` 分支早就把"收下却不处理"当成对用户撒谎（V12 R4-e）。
//! 这里把两侧并成一条口径：多余的旗标与多余的位置参数一律报用法（V13 R2 #204）。
//! `paper`/`paper-check` 的 `--json` 也按同一标准拒绝 —— `cli_args.rs` 的
//! `RunCommand::machine_output` 从来不为这三条入口生成机器可读输出。

use crate::*;

/// 读一条单配置文件入口的参数：`(配置文件路径, 是否要机器可读输出)`。
///
/// `json_output` 是逐入口的能力位，不是调用方的偏好：外层不给它生成 JSON 时，
/// 这里就必须把 `--json` 顶回去，否则旗标又一次"收下却不处理"。
pub(crate) fn run_entry_arguments(
    arguments: &[String],
    entry: &str,
    json_output: bool,
    default: impl FnOnce() -> PathBuf,
) -> Result<(PathBuf, bool), String> {
    let mut path: Option<&str> = None;
    let mut json = false;
    for value in arguments.iter().skip(1).map(String::as_str) {
        if value == "--json" {
            if !json_output {
                return Err(run_usage(&format!(
                    "{entry} 不接受旗标 --json（这条入口没有机器可读输出）"
                )));
            }
            json = true;
            continue;
        }
        if value.starts_with('-') || path.is_some() {
            return Err(run_usage(&format!(
                "{entry} 只接受一个可选的配置文件路径与 --json，不接受 {value}"
            )));
        }
        path = Some(value);
    }
    Ok((path.map(PathBuf::from).unwrap_or_else(default), json))
}

/// 读 `run report` 的专属旗标；此入口额外支持 `--html --output <path>` 与 `--evidence`。
pub(crate) fn run_report_entry_arguments(
    arguments: &[String],
    default: impl FnOnce() -> PathBuf,
) -> Result<(PathBuf, bool, bool, bool, Option<PathBuf>), String> {
    let mut path: Option<&str> = None;
    let mut output = None;
    let mut json = false;
    let mut html = false;
    let mut evidence = false;
    let mut values = arguments.iter().skip(1);
    while let Some(value) = values.next() {
        match value.as_str() {
            "--json" if json => return Err(run_usage("report 的 --json 只能指定一次")),
            "--json" => json = true,
            "--html" if html => return Err(run_usage("report 的 --html 只能指定一次")),
            "--html" => html = true,
            "--evidence" if evidence => return Err(run_usage("report 的 --evidence 只能指定一次")),
            "--evidence" => evidence = true,
            "--output" | "-o" => {
                if output.is_some() {
                    return Err(run_usage("report 的 --output 只能指定一次"));
                }
                let Some(value) = values.next().filter(|value| !value.starts_with('-')) else {
                    return Err(run_usage("report --output 需要一个路径值"));
                };
                output = Some(PathBuf::from(value));
            }
            value if value.starts_with("--output=") => {
                if output.is_some() {
                    return Err(run_usage("report 的 --output 只能指定一次"));
                }
                let path = value.trim_start_matches("--output=");
                if path.is_empty() {
                    return Err(run_usage("report --output 需要一个路径值"));
                }
                output = Some(PathBuf::from(path));
            }
            value if value.starts_with("-o=") => {
                if output.is_some() {
                    return Err(run_usage("report 的 --output 只能指定一次"));
                }
                let path = value.trim_start_matches("-o=");
                if path.is_empty() {
                    return Err(run_usage("report --output 需要一个路径值"));
                }
                output = Some(PathBuf::from(path));
            }
            value if value.starts_with('-') => {
                return Err(run_usage(&format!("report 不接受旗标 {value}")));
            }
            value if path.is_none() => path = Some(value),
            value => {
                return Err(run_usage(&format!(
                    "report 只接受一个可选路径，不接受 {value}"
                )))
            }
        }
    }
    if output.is_some() && !html {
        return Err(run_usage("report --output 必须与 --html 一起使用"));
    }
    Ok((
        path.map(PathBuf::from).unwrap_or_else(default),
        json,
        html,
        evidence,
        output,
    ))
}

/// 把入口名放回参数向量头部：`run <入口> …` 的**子入口**在 clap 层已被摘掉，
/// 而下游统一处理器（`config_commands.rs` 的 `run_unified_command`）按「第一条即入口名」
/// 读它，所以这里补回去——读法与写方同住一层，别处再抄一份就会漂。
pub(crate) fn run_arguments(entry: &str, mut arguments: Vec<String>) -> Vec<String> {
    arguments.insert(0, entry.to_string());
    arguments
}
