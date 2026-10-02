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
