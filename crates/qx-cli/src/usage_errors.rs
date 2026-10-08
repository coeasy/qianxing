//! 用法错误的分级回显（U2/U4）：错误正文 + 两行指路，取代整篇入口摘要墙。
//!
//! 改前实测：`qx-cli --version`、`qx-cli nope` 这类用法错误会先打印 162 行 / 12 KB 的
//! 入口摘要再退 2，新手要在一面墙里找自己那半行错。clap 的报错正文本身已经交出需要的
//! 三格：错误行、`tip:` 近似入口名（改前 `version` → `verify`，改后 `versoin` → `version`）、该入口的 `Usage:` 形状
//! （`doctor --config x` → `qx-cli.exe doctor [OPTIONS] [PATH]`）。所以本轮不是"再写一份
//! 建议算法"，而是停止追加摘要：完整入口只从 `help`/`--help`/`-h` 三条显式出口走。

use qx_core::QxError;

/// 这一行前缀同时含「未知命令」与「未知参数」：`tests/cli_dispatch.rs` 与
/// `tests/multi_leg_attribution/entries.rs` 分别点名其中一种，缺一即用例红。
const ERROR_PREFIX: &str = "未知命令或未知参数";

/// 打印分级回显并以退出码 2 fail closed（与迁移前的退出码一致）。
pub(crate) fn report(error: &clap::Error) -> ! {
    eprintln!("{ERROR_PREFIX}: {error}");
    eprintln!("下一步: 全部入口看 `qx-cli help`，单条用法看 `qx-cli <命令> --help`");
    eprintln!("自证构建: `qx-cli version`（等价 `--version` 与 `-V`）");
    std::process::exit(2);
}

/// 业务处理器失败的统一出口：一行原因 + 退出码 2。与各派发臂此前的
/// `if let Err(error) = … { eprintln!("{原因}: {error}"); exit(2) }` 逐字等价，
/// 只是把这段形状收成一处，免得每个数据入口各抄一遍。
pub(crate) fn exit_on_failure(result: Result<(), String>, what: &str) {
    if let Err(error) = result {
        eprintln!("{what}: {error}");
        std::process::exit(2);
    }
}

/// `QxError` 到 CLI 诊断文本的统一出口（DD-5 / P1-11）。
///
/// 此前这些地方写的是 `format!("{上下文}: {error}")`——`Display` 恰好带 `[CODE]`，但
/// "要不要先对账、能不能重试"这两格随错误一起丢掉了，机器读到的只有一句中文。这里改成
/// **显式消费错误码五元契约**：码一定在，且按契约补一句下一步。调用方不要再手写
/// `format!("…: {error}")`——那样又会退回"只有消息、没有口径"。
///
/// 三档提示按契约的 `retryability` 分：结果未知要先对账（`Ambiguous`/`ReconcileRequired`）；
/// 允许立即重试（`Transient`）；只能在退避后重试（`VenueState`/`ResourceExhausted`）。
/// 业务拒绝与内部不变量**不给提示**——给了就是在暗示调用方重发。
pub(crate) fn qx_context(context: &str, error: &QxError) -> String {
    let contract = error.contract();
    let hint = if contract.reconcile_required {
        "（结果未知，需先对账）"
    } else if contract.retryability.allows_retry() {
        "（可重试）"
    } else if contract.retryability.may_retry_eventually() {
        "（需退避后重试）"
    } else {
        ""
    };
    format!(
        "[{}] {context}: {}{hint}",
        contract.code, contract.user_message
    )
}
