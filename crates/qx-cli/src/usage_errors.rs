//! 用法错误的分级回显（U2/U4）：错误正文 + 两行指路，取代整篇入口摘要墙。
//!
//! 改前实测：`qx-cli --version`、`qx-cli nope` 这类用法错误会先打印 162 行 / 12 KB 的
//! 入口摘要再退 2，新手要在一面墙里找自己那半行错。clap 的报错正文本身已经交出需要的
//! 三格：错误行、`tip:` 近似入口名（改前 `version` → `verify`，改后 `versoin` → `version`）、该入口的 `Usage:` 形状
//! （`doctor --config x` → `qx-cli.exe doctor [OPTIONS] [PATH]`）。所以本轮不是"再写一份
//! 建议算法"，而是停止追加摘要：完整入口只从 `help`/`--help`/`-h` 三条显式出口走。

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
