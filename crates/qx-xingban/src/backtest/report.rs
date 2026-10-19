use crate::{AshareIssuerCapitalSnapshot, ProtectiveExitFill};
use qx_core::{EventLog, Ledger};

pub struct BacktestReport {
    pub fills: Vec<qx_core::Fill>,
    /// One model evaluation per eligible pending order and market observation.
    pub execution_attempts: Vec<crate::ExecutionAttempt>,
    pub equity: Vec<i128>,
    /// 现金基准曲线；没有外部基准序列时显式标记为 flat-cash。
    pub benchmark_equity: Vec<i128>,
    pub positions: Vec<i128>,
    /// Protective exits are keyed to their completed-bar trigger and actual next-bar fills.
    pub protective_exit_fills: Vec<ProtectiveExitFill>,
    /// 回测期间生效的发行人股本事实；它不属于账户 Ledger。
    pub issuer_capital_snapshots: Vec<AshareIssuerCapitalSnapshot>,
    pub fees_raw: i128,
    pub turnover_raw: i128,
    pub max_drawdown_raw: i128, // 衍生于 max_drawdown_bps；登记了零生产读者 limitation。
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub assumptions: Vec<String>,
    pub event_log: EventLog,
    pub ledger: Ledger,
    pub fill_model_descriptor: crate::FillModelDescriptor,
    pub model_descriptors: Vec<String>,
    pub seed: u64,
    pub input_data_hash: u64,
    pub clock_start: u64,
    pub clock_end: u64,
}
