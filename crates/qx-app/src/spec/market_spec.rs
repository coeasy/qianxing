use crate::error::{AppError, AppErrorCategory};
use crate::spec::{BacktestMarginConfig, BacktestMarginTier};
use qx_core::{InstrumentId, TradingInstrumentSpec, TradingProduct};
use serde_json::Value;

pub const CCXT_MARKET_SPEC_VERSION: &str = "ccxt-market-spec-v1";
pub const PRODUCT_MARKET_SPEC_VERSION: &str = "product-market-spec-v1";

fn market_value_is_product_spec(market: &Value) -> bool {
    market.get("base_currency").is_some()
}

pub fn market_spec_source_label(market: &Value) -> &'static str {
    if market_value_is_product_spec(market) {
        PRODUCT_MARKET_SPEC_VERSION
    } else {
        CCXT_MARKET_SPEC_VERSION
    }
}

/// Parse the two supported market snapshot shapes into the frozen domain contract.
pub fn market_spec_from_value(
    instrument: &InstrumentId,
    market: &Value,
) -> Result<TradingInstrumentSpec, AppError> {
    let spec = if market_value_is_product_spec(market) {
        let spec: TradingInstrumentSpec =
            serde_json::from_value(market.clone()).map_err(|error| {
                invalid(format!(
                    "产品规格 market spec 反序列化失败（带 base_currency 的那种形状）: {error}"
                ))
            })?;
        if spec.instrument != *instrument {
            return Err(invalid(format!(
                "market spec 的 instrument {} 与标的 {instrument} 不一致",
                spec.instrument
            )));
        }
        spec
    } else {
        ccxt_market_to_spec(instrument, market)?
    };
    spec.validate()
        .map_err(|error| invalid(format!("market spec 字段非法: {error:?}")))?;
    Ok(spec)
}

fn ccxt_market_to_spec(
    instrument: &InstrumentId,
    market: &Value,
) -> Result<TradingInstrumentSpec, AppError> {
    let market_type = market
        .get("market_type")
        .and_then(Value::as_str)
        .unwrap_or("spot");
    let product = match market_type {
        "spot" => TradingProduct::Spot,
        "margin" => TradingProduct::Margin,
        "swap" | "perpetual" => TradingProduct::Perpetual,
        "future" | "futures" => TradingProduct::Future,
        "option" | "options" => TradingProduct::Option,
        other => return Err(invalid(format!("CCXT market type 不支持: {other}"))),
    };
    let base_currency = market
        .get("base")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid("CCXT market 缺少 base"))?;
    let quote_currency = market
        .get("quote")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid("CCXT market 缺少 quote"))?;
    let settlement_currency = market
        .get("settle")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(quote_currency);
    let contract_size = raw_i128(market, "contract_size_raw")?;
    let linear = market
        .get("linear")
        .and_then(Value::as_bool)
        .unwrap_or(!product.is_derivative());
    let inverse = market
        .get("inverse")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_leverage = match market
        .get("max_leverage")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
    {
        Some(0) => return Err(invalid("CCXT market max_leverage 不能为 0")),
        Some(value) => value,
        None if product.is_derivative() => {
            return Err(declared_field_problem(
                "max_leverage",
                "衍生品杠杆上限",
                "100x",
            ));
        }
        None => 1,
    };
    let maintenance_margin_bps = match market
        .get("maintenance_margin_bps")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
    {
        Some(value) if value <= 10_000 => value,
        Some(value) => {
            return Err(invalid(format!(
                "CCXT market maintenance_margin_bps 非法: {value}"
            )));
        }
        None if product.is_derivative() => {
            return Err(declared_field_problem(
                "maintenance_margin_bps",
                "衍生品维持保证金率",
                "500bp",
            ));
        }
        None => 0,
    };
    Ok(TradingInstrumentSpec {
        instrument: instrument.clone(),
        product,
        base_currency: base_currency.into(),
        quote_currency: quote_currency.into(),
        settlement_currency: settlement_currency.into(),
        contract_size,
        linear,
        inverse,
        price_tick: declared_raw_number(market, "price_tick_raw", "一档价格精度")?,
        qty_step: declared_raw_number(market, "qty_step_raw", "数量步长")?,
        min_qty: declared_raw_number(market, "min_qty_raw", "最小下单量")?,
        max_leverage,
        maintenance_margin_bps,
        valid_from: 0,
        valid_to: market.get("expiry_ms").and_then(Value::as_u64),
    })
}

fn declared_raw_number(market: &Value, key: &str, what: &str) -> Result<i128, AppError> {
    let declared =
        raw_i128(market, key).map_err(|_| declared_field_problem(key, what, "1 个最小报价单位"))?;
    if declared <= 0 {
        return Err(invalid(format!(
            "CCXT market {key} 必须为正，实际 {declared}"
        )));
    }
    Ok(declared)
}

fn raw_i128(value: &Value, key: &str) -> Result<i128, AppError> {
    if let Some(value) = value.get(key).and_then(Value::as_i64) {
        return Ok(i128::from(value));
    }
    if let Some(value) = value.get(key).and_then(Value::as_u64) {
        return Ok(i128::from(value));
    }
    if let Some(value) = value.get(key).and_then(Value::as_str) {
        return value
            .parse::<i128>()
            .map_err(|_| invalid(format!("CCXT 字段不是定点整数: {key}")));
    }
    Err(invalid(format!("CCXT 字段不是定点整数: {key}")))
}

fn declared_field_problem(key: &str, what: &str, fallback: &str) -> AppError {
    invalid(format!(
        "CCXT market 缺少 {key}（{what}）：兜底成 {fallback} 会让闸门和滑点口径静默失效，\
         而产物里没有任何地方写着它是猜的。交易所未报这一口径时请改传已冻结的产品规格 \
         market spec（带 base_currency/instrument 的那种形状）。"
    ))
}

/// Convert normalized CCXT leverage bands into the serializable application margin binding.
pub fn backtest_margin_model_from_market(market: &Value) -> BacktestMarginConfig {
    let Some(rows) = market.get("leverage_tiers").and_then(Value::as_array) else {
        return BacktestMarginConfig::NoMargin;
    };
    let tiers = rows
        .iter()
        .filter_map(|row| {
            let max_notional = row
                .get("max_notional_raw")
                .and_then(Value::as_i64)
                .map(i128::from)
                .or_else(|| {
                    row.get("max_notional_raw")
                        .and_then(Value::as_u64)
                        .map(i128::from)
                })
                .unwrap_or(i128::MAX);
            let initial_bp = row
                .get("initial_margin_bps")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let maintenance_bp = row
                .get("maintenance_margin_bps")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let max_leverage = row
                .get("max_leverage")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok());
            (max_notional > 0 && initial_bp >= 0 && maintenance_bp >= 0).then_some(
                BacktestMarginTier {
                    max_notional,
                    initial_bp,
                    maintenance_bp,
                    max_leverage,
                },
            )
        })
        .collect::<Vec<_>>();
    if tiers.is_empty() {
        BacktestMarginConfig::NoMargin
    } else {
        BacktestMarginConfig::Tiers { tiers }
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
