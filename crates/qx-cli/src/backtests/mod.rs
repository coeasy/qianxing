//! 回测编排：策略回测、深度档回测、多标的多腿回测与产物落盘。
//!
//! 撮合内核来自 qx-xingban，本模块负责输入解析、风控门同源注入与运行清单/产物持久化。

pub(crate) use super::{
    multi_leg_integrity_summary, read_example_json, read_runtime_config, rejection_count,
    rejection_facts_line, render_number, verify_strategy_artifact, Bar, BarFrame,
    BuiltinStrategyKind, ContractBarStrategy, InstrumentId, JsonDatasetRegistry, Quantity,
    RuntimeConfig, MULTI_LEG_KINDS,
};
pub(crate) use qx_app::BAR_MATCHING_KERNEL;
use std::path::Path;

pub(crate) use qx_app::BacktestMarketSpecBinding as MarketSpecLoad;

// 没人声明本金时，单腿回测按多少记账、这条必答题怎么答：见 `account_base.rs`（V11 Q72）。

/// market spec JSON → application binding；CLI 保留文件读取与错误路径。
pub(crate) fn market_spec_with_margin(
    instrument: &InstrumentId,
    spec_path: Option<&Path>,
    label: &str,
) -> Result<MarketSpecLoad, String> {
    let market = spec_path
        .map(|spec_path| {
            let payload = read_example_json(spec_path, &format!("{label} market spec "))?;
            serde_json::from_str::<serde_json::Value>(&payload).map_err(|error| {
                format!(
                    "{label} market spec JSON 无效 {}: {error}",
                    spec_path.display()
                )
            })
        })
        .transpose()?;
    qx_app::backtest_market_spec_binding(instrument, market.as_ref()).map_err(|error| {
        match spec_path {
            Some(path) => format!("{label} market spec {}: {error}", path.display()),
            None => format!("{label} market spec: {error}"),
        }
    })
}

mod account_base;
pub(crate) use account_base::*;

mod artifacts;
pub(crate) use artifacts::*;

mod builtin_app;
pub(crate) use builtin_app::*;

mod ashare_binding;
pub(crate) use ashare_binding::*;
mod ccxt_builtin;
pub(crate) use ccxt_builtin::*;

mod depth;
pub(crate) use depth::*;

mod fast_backtest;
pub(crate) use fast_backtest::*;

mod multi_builtin;
pub(crate) use multi_builtin::*;

mod summary_listing;
pub(crate) use summary_listing::*;

mod single_strategy;
pub(crate) use single_strategy::*;

mod strategy_backtest;
pub(crate) use strategy_backtest::*;

pub(crate) fn read_bar_frame_for_multi_backtest(
    path: &Path,
    label: &str,
) -> Result<BarFrame, String> {
    let payload = read_example_json(path, &format!("{label} BarFrame "))?;
    BarFrame::from_json(&payload)
        .map_err(|error| format!("{label} BarFrame 校验失败 {}: {error:?}", path.display()))
}
