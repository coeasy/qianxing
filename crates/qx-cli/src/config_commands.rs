//! 配置与运维查询命令面：`config`/`status`/`doctor`/`report`/`live-check` 与 `run` 统一派发。
//! 只做配置读取、引用校验与人读/机读输出；帮助文本见 `cli_help.rs`，初始化一族见 `init_project.rs`。

use crate::{
    build_identity, check_account_log_settlement, check_orphan_event_logs, check_storage_data_dir,
    effective_storage_root, latest_backtest_readout_lines, list_backtest_summary_paths,
    read_runtime_config, repository_deploy_path, run_entry_arguments, run_live_check,
    run_paper_check, run_paper_pipeline_once, run_runtime_check, run_unified_backtest,
    run_unified_report_entry, run_usage, validate_runtime_references, worker_credentials_ready,
    writes_account_ledger,
};
use qx_runtime::{RuntimeConfig, RuntimeSupervisor};
use std::path::{Path, PathBuf};

pub(crate) fn default_runtime_path() -> PathBuf {
    repository_deploy_path("qianxing.runtime.example.json")
}

/// `run` 统一入口的**单一**默认运行时：与 `run backtest` 用的是同一份。
///
/// 这不是口味问题。`run` 自称"统一执行常用安全入口"（`cli_help.rs`），而它最常用的一对是
/// `run backtest` 写产物、`run report` 读产物——两者落到不同的 `storage.data_dir` 时，刚写出的
/// 摘要另一端一个都找不到。实测（V13 R28）：`run backtest` 写进
/// `data/qianxing-builtin-strategy/runs`，紧接着 `run report` 报
/// `未找到回测摘要: data/qianxing\runs` 并以 2 退出，而两份模板的差别只是默认运行时不同
/// （`run backtest` 走 `qianxing.runtime.builtin-strategy.example.json`，其余数据面子命令走
/// [`default_runtime_path`] 的 `qianxing.runtime.example.json`）。
///
/// 为什么统一到内置策略那一份而不是反过来：`main.rs` 的 `run_unified_backtest` 特意把默认从
/// 跨语言策略模板换成内置策略模板（V13 R31 ① F6）——README 安装路径 A 只装 CLI 不装 Python，
/// 旧默认那份策略由 Python worker 承担，`qx-cli backtest` 一敲就撞解释器。所以"链条的写侧"
/// 已经定在内置策略那一份，读侧跟上；`default_runtime_path` 保持给顶层命令（`config`/`status`/
/// 顶层 `doctor`/`report`）用，那里没有"写侧与读侧配对"的关系。
pub(crate) fn unified_default_runtime_path() -> PathBuf {
    repository_deploy_path("qianxing.runtime.builtin-strategy.example.json")
}

mod configuration;
pub(crate) use configuration::{run_config_explain, run_config_fingerprint, run_config_lock};

pub(crate) fn run_unified_command(arguments: &[String]) -> Result<(), String> {
    let action = arguments
        .first()
        .map(String::as_str)
        .ok_or_else(|| run_usage("需要入口参数"))?;
    match action {
        "backtest" => {
            let positional: Vec<String> = arguments[1..]
                .iter()
                .filter(|value| !value.starts_with('-'))
                .cloned()
                .collect();
            // 收下却不处理等于对用户撒谎：旗标与多余位置参数都报用法，而不是静默丢弃。
            if arguments.len() - 1 > positional.len() || positional.len() > 3 {
                return Err(run_usage(
                    "backtest 不接受旗标，位置参数至多三个：<runtime.json> [bar-frame.json] [market-spec.json]",
                ));
            }
            run_unified_backtest(
                positional.first().map(PathBuf::from).as_deref(),
                positional.get(1).map(PathBuf::from).as_deref(),
                positional.get(2).map(PathBuf::from).as_deref(),
            )
        }
        "paper" => {
            let (path, _) = run_entry_arguments(arguments, action, false, || {
                repository_deploy_path("qianxing.runtime.paper-strategy.example.json")
            })?;
            run_paper_pipeline_once(&path)
        }
        "paper-check" => {
            let (path, json) = run_entry_arguments(arguments, action, true, || {
                repository_deploy_path("qianxing.runtime.paper-strategy.example.json")
            })?;
            run_paper_check(&path, json)
        }
        "doctor" => {
            // 数据面三入口共用 `unified_default_runtime_path`：缺省路径若与 `run backtest` 的
            // 写侧不同，链条就在这里断（见该函数的文档）。
            let (path, json) =
                run_entry_arguments(arguments, action, true, unified_default_runtime_path)?;
            run_doctor(&path, json)
        }
        "live-check" => {
            let (path, json) = run_entry_arguments(arguments, action, true, || {
                repository_deploy_path("qianxing.runtime.production.example.json")
            })?;
            run_live_check(&path, json)
        }
        "runtime-check" => {
            let (path, json) =
                run_entry_arguments(arguments, action, true, unified_default_runtime_path)?;
            run_runtime_check(&path, json)
        }
        "report" => run_unified_report_entry(arguments, unified_default_runtime_path),
        _ => Err(run_usage(&format!("不支持 {action}"))),
    }
}

mod status;
pub(crate) use status::{resolve_backtest_summary_path, run_status};

mod doctor;
pub(crate) use doctor::{
    collect_doctor_report, collect_doctor_report_for_config, run_doctor, run_doctor_capabilities,
};
