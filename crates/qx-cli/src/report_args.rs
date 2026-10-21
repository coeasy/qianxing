//! `report` 的 CLI 参数载体，避免顶层 clap 命令表继续膨胀。

use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub(crate) struct ReportArgs {
    pub(crate) path: Option<PathBuf>,
    #[arg(long)]
    pub(crate) json: bool,
    #[arg(long)]
    pub(crate) html: bool,
    /// 复核通过后再生成对应运行的 EvidenceBundle。
    #[arg(long)]
    pub(crate) evidence: bool,
    #[arg(short = 'o', long = "output", value_name = "PATH")]
    pub(crate) out: Option<PathBuf>,
    /// 与主摘要比较；可重复指定，所有摘要须绑定同一标的和数据指纹。
    #[arg(long = "against", value_name = "SUMMARY")]
    pub(crate) against: Vec<PathBuf>,
    /// 用交易日/Bar 时间戳完全对齐的基准 BarFrame 做相对收益分析；使用原始 close，不做复权。
    #[arg(long = "benchmark-bars", value_name = "BAR_FRAME")]
    pub(crate) benchmark_bars: Option<PathBuf>,
    /// 可选 A 股交易日历；启用时只接受每个交易日一条且完整覆盖的日线序列。
    #[arg(long = "benchmark-calendar", value_name = "CALENDAR_JSON")]
    pub(crate) benchmark_calendar: Option<PathBuf>,
    /// 将逐 Bar 组合/基准/主动收益序列导出为 CSV。
    #[arg(long = "benchmark-csv", value_name = "PATH")]
    pub(crate) benchmark_csv: Option<PathBuf>,
    /// 输出包含摘要、权益、成交、月度收益和可选基准序列的 Excel 工作簿。
    #[arg(long = "xlsx", value_name = "PATH")]
    pub(crate) xlsx: Option<PathBuf>,
}
