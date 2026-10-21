//! Shared OHLCV fill-model validation and binding projection.

use crate::BacktestFillModel;
use qx_core::TradingInstrumentSpec;

const REACHABLE_FILL_MODELS: [BacktestFillModel; 3] = [
    BacktestFillModel::NextBarOpen,
    BacktestFillModel::BestPrice,
    BacktestFillModel::OneTickSlippage,
];

#[derive(Clone, Copy, Debug)]
pub struct BarFillModelBinding {
    pub model: BacktestFillModel,
    pub name: &'static str,
    pub source: &'static str,
}

fn fill_model_name(kind: BacktestFillModel) -> &'static str {
    match kind {
        BacktestFillModel::NextBarOpen => "next_bar_open",
        BacktestFillModel::BestPrice => "best_price",
        BacktestFillModel::OneTickSlippage => "one_tick_slippage",
    }
}

fn reachable_fill_models() -> String {
    REACHABLE_FILL_MODELS
        .iter()
        .copied()
        .map(fill_model_name)
        .collect::<Vec<_>>()
        .join(" / ")
}

fn parse_fill_model(configured: &str) -> Result<BacktestFillModel, String> {
    let trimmed = configured.trim();
    if let Some(kind) = REACHABLE_FILL_MODELS
        .into_iter()
        .find(|kind| fill_model_name(*kind) == trimmed)
    {
        return Ok(kind);
    }
    let missing_tier = match trimmed {
        "probabilistic" => Some("L1 一档盘口"),
        "volume_sensitive" => Some("L2/L3 深度盘口"),
        _ => None,
    };
    Err(match missing_tier {
        Some(tier) => format!(
            "撮合模型 {trimmed} 需要{tier}，而 Bar 回测的输入只有 OHLCV：装上它等于声称还原了\
             并不存在的盘口。深度链（backtest book）走 Tick/OrderBook 内核、不经过 FillModel，\
             所以它暂时没有任何回测入口（V11 §15.4 第 1 条）。strategy.fill_model 只接受 {}。",
            reachable_fill_models()
        ),
        None => format!(
            "运行时配置 strategy.fill_model 未知: {configured}（只接受 {}）",
            reachable_fill_models()
        ),
    })
}

pub fn bar_fill_model(
    configured: Option<&str>,
    instrument_spec: Option<&TradingInstrumentSpec>,
) -> Result<BarFillModelBinding, String> {
    let kind = match configured {
        Some(configured) => parse_fill_model(configured)?,
        None => BacktestFillModel::NextBarOpen,
    };
    if kind == BacktestFillModel::OneTickSlippage {
        declared_price_tick(fill_model_name(kind), instrument_spec)?;
    }
    Ok(BarFillModelBinding {
        model: kind,
        name: fill_model_name(kind),
        source: if configured.is_some() {
            "runtime-config"
        } else {
            "builtin-default"
        },
    })
}

fn declared_price_tick(
    name: &str,
    instrument_spec: Option<&TradingInstrumentSpec>,
) -> Result<i128, String> {
    match instrument_spec {
        Some(spec) if spec.price_tick > 0 => Ok(spec.price_tick),
        Some(spec) => Err(format!(
            "撮合模型 {name} 的一档滑点取自 market spec，但它的 price_tick={} 不是正数",
            spec.price_tick
        )),
        None => Err(format!(
            "撮合模型 {name} 的一档滑点只认 market spec 的 price_tick，本次回测没有加载 market \
             spec，拒绝为标的猜一档：请给本入口传 market spec 文件，或改用 {} / {}",
            fill_model_name(BacktestFillModel::NextBarOpen),
            fill_model_name(BacktestFillModel::BestPrice)
        )),
    }
}

pub fn fill_model_problem(configured: &str) -> Option<String> {
    parse_fill_model(configured).err()
}

pub fn fill_model_failure(configured: Option<&str>, label: &str) -> Option<String> {
    configured
        .and_then(fill_model_problem)
        .map(|problem| format!("{label}.fill_model {problem}"))
}
