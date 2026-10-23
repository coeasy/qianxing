use crate::error::{AppError, AppErrorCategory};
use crate::spec::{BacktestMarginConfig, BacktestMarginTier};
use qx_core::{InstrumentId, TradingInstrumentSpec, TradingProduct};
use serde_json::Value;

pub const CCXT_MARKET_SPEC_VERSION: &str = "ccxt-market-spec-v1";
pub const PRODUCT_MARKET_SPEC_VERSION: &str = "product-market-spec-v1";
pub const DEFAULT_INSTRUMENT_SPEC_VERSION: &str = "default-instrument-spec-v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BacktestMarketSpecBinding {
    pub spec: Option<TradingInstrumentSpec>,
    pub app_margin_model: Option<BacktestMarginConfig>,
    pub source: &'static str,
}

/// Bind an optional market snapshot to every market-dependent backtest input in one place.
pub fn backtest_market_spec_binding(
    instrument: &InstrumentId,
    market: Option<&Value>,
) -> Result<BacktestMarketSpecBinding, AppError> {
    let Some(market) = market else {
        return Ok(BacktestMarketSpecBinding {
            spec: None,
            app_margin_model: None,
            source: DEFAULT_INSTRUMENT_SPEC_VERSION,
        });
    };

    Ok(BacktestMarketSpecBinding {
        spec: Some(market_spec_from_value(instrument, market)?),
        app_margin_model: Some(backtest_margin_model_from_market(market)),
        source: market_spec_source_label(market),
    })
}

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
    if let Some(exchange_id) = market.get("exchange_id").and_then(Value::as_str) {
        if !exchange_id.trim().is_empty()
            && !exchange_id.eq_ignore_ascii_case(instrument.venue.as_str())
        {
            return Err(invalid(format!(
                "CCXT market exchange_id {exchange_id} 与 InstrumentId venue {} 不一致",
                instrument.venue
            )));
        }
    }
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
        min_notional_raw: optional_nonnegative_raw(market, "min_notional_raw")?.unwrap_or(0),
        max_leverage,
        maintenance_margin_bps,
        valid_from: 0,
        valid_to: market.get("expiry_ms").and_then(Value::as_u64),
    })
}

fn optional_nonnegative_raw(market: &Value, key: &str) -> Result<Option<i128>, AppError> {
    let Some(value) = market.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let raw = raw_i128(market, key)?;
    if raw < 0 {
        return Err(invalid(format!("CCXT market {key} 不能为负数")));
    }
    Ok(Some(raw))
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

#[cfg(test)]
mod binding_tests {
    use super::*;

    #[test]
    fn absent_snapshot_returns_the_default_binding() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").expect("valid instrument");
        let binding = backtest_market_spec_binding(&instrument, None).expect("default binding");

        assert_eq!(binding.spec, None);
        assert_eq!(binding.app_margin_model, None);
        assert_eq!(binding.source, DEFAULT_INSTRUMENT_SPEC_VERSION);
    }

    #[test]
    fn product_snapshot_binds_spec_margin_and_source_together() {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").expect("valid instrument");
        let market = serde_json::json!({
            "instrument": {"symbol": "BTCUSDT", "venue": "BINANCE"},
            "product": "spot",
            "base_currency": "BTC",
            "quote_currency": "USDT",
            "settlement_currency": "USDT",
            "contract_size": 1_000_000_000_i64,
            "linear": true,
            "inverse": false,
            "price_tick": 1_000_000_i64,
            "qty_step": 1_000_000_i64,
            "min_qty": 1_000_000_i64,
            "max_leverage": 1,
            "maintenance_margin_bps": 0,
            "valid_from": 0,
            "valid_to": null
        });
        let binding = backtest_market_spec_binding(&instrument, Some(&market))
            .expect("valid product market binding");

        assert_eq!(
            binding.spec.as_ref().map(|spec| &spec.instrument),
            Some(&instrument)
        );
        assert_eq!(
            binding.app_margin_model,
            Some(BacktestMarginConfig::NoMargin)
        );
        assert_eq!(binding.source, PRODUCT_MARKET_SPEC_VERSION);
    }

    #[test]
    fn ccxt_snapshot_preserves_small_ticks_tiers_and_exchange_identity() {
        let instrument = InstrumentId::parse("BTC/USDT:USDT.OKX").expect("valid instrument");
        let market = serde_json::json!({
            "exchange_id": "okx",
            "symbol": "BTC/USDT:USDT",
            "market_id": "BTC-USDT-SWAP",
            "base": "BTC",
            "quote": "USDT",
            "market_type": "swap",
            "active": true,
            "contract": true,
            "linear": true,
            "inverse": false,
            "contract_size_raw": 1_000_000_000_i64,
            "settle": "USDT",
            "expiry_ms": null,
            "price_tick_raw": 1,
            "qty_step_raw": 1_000,
            "min_qty_raw": 1_000,
            "min_notional_raw": 5_000_000_000_i64,
            "max_qty_raw": null,
            "max_leverage": 20,
            "maintenance_margin_bps": 50,
            "leverage_tiers": [{
                "symbol": "BTC/USDT:USDT",
                "tier": 1,
                "min_notional_raw": 0,
                "max_notional_raw": 100_000_000_000_000_i64,
                "initial_margin_bps": 1_000,
                "maintenance_margin_bps": 50,
                "max_leverage": 20
            }]
        });

        let binding = backtest_market_spec_binding(&instrument, Some(&market))
            .expect("normalized CCXT snapshot binds into the product contract");
        let spec = binding.spec.expect("product spec");
        assert_eq!(spec.price_tick, 1);
        assert_eq!(spec.qty_step, 1_000);
        assert_eq!(spec.min_notional_raw, 5_000_000_000);
        assert_eq!(spec.product, TradingProduct::Perpetual);
        assert!(matches!(
            binding.app_margin_model,
            Some(BacktestMarginConfig::Tiers { .. })
        ));

        let mut wrong_venue = market.clone();
        wrong_venue["exchange_id"] = serde_json::Value::String("bitget".into());
        assert!(market_spec_from_value(&instrument, &wrong_venue)
            .unwrap_err()
            .message()
            .contains("venue OKX 不一致"));

        let mut negative_minimum = market;
        negative_minimum["min_notional_raw"] = serde_json::json!(-1);
        assert!(market_spec_from_value(&instrument, &negative_minimum)
            .unwrap_err()
            .message()
            .contains("不能为负数"));
    }
}
