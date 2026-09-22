//! 产品规格（market spec）JSON 的唯一读法，以及 CCXT 归一化快照到产品规格的转换。
//!
//! 回测链与 live/paper worker 都经由 [market_spec_from_value] 解析同一份文件；把这一
//! 口径单独成模块，是为了让两种形状各养一套解析没有第二个落点。

use super::*;
/// 两种形状的唯一判定点。产物上的来源标签（[market_spec_source_label]）与这里的解析
/// 必须问同一个问题，否则"按哪种形状读的"会在标签上被写反（V11 Q63）。
pub(crate) fn market_value_is_product_spec(market: &serde_json::Value) -> bool {
    market.get("base_currency").is_some()
}

/// 规格来源标签，写进 `RunManifest.instrument_spec_version`。
///
/// 旧口径只问"有没有传 `--market-spec`"，于是产品规格形状的文件也被标成 `ccxt-market-spec-v1`：
/// 两份形状不同、`contract_size` 与精度不同的文件在同一帧同一配置下得到同一个标签，而规格内容
/// 并不进 `model_fingerprint`，这个字段是唯一承载"规格从哪来、按哪种形状解析"的地方。
pub(crate) fn market_spec_source_label(market: &serde_json::Value) -> &'static str {
    if market_value_is_product_spec(market) {
        PRODUCT_MARKET_SPEC_VERSION
    } else {
        CCXT_MARKET_SPEC_VERSION
    }
}

pub(crate) const CCXT_MARKET_SPEC_VERSION: &str = "ccxt-market-spec-v1";
pub(crate) const PRODUCT_MARKET_SPEC_VERSION: &str = "product-market-spec-v1";
pub(crate) const DEFAULT_INSTRUMENT_SPEC_VERSION: &str = "default-instrument-spec-v1";

/// 一次 market spec 读取要交付的三件事：合约规格、保证金规则，以及规格**从哪来**。
///
/// 三者同源于同一份文件、同一处判定，拆开返回就会在调用点上被读成三件独立的事——
/// 而"来源"一旦被读成"有没有传路径"，产物上那一行就开始说谎（V11 Q63）。
pub(crate) struct MarketSpecLoad {
    pub(crate) spec: Option<TradingInstrumentSpec>,
    pub(crate) margin: Box<dyn MarginRule>,
    /// 写进 `RunManifest.instrument_spec_version` 的那一行，取自 [market_spec_source_label]
    /// 实际判定出的形状；没给文件时是上面那三档常量里的 `default-instrument-spec-v1`。
    pub(crate) source: &'static str,
}

/// market spec JSON 的**唯一**读法：回测链与 live/paper worker 从同一份口径解析同一份文件。
///
/// 两种形状都是合法输入，靠 `base_currency` 是否存在来分辨，而不是"先按 A 解、失败再按 B
/// 解"：后者会把一份写错的产品规格误报成"CCXT market 缺少 base"，让人去补一个根本不适用
/// 的字段。分裂的代价在 `init` 上直接暴露过——它打印的下一步回测命令带的就是产品规格形状
/// 的那份 `qianxing.binance.spot.spec.json`，回测链当时只认 CCXT 形状，于是首条命令必失败。
pub(crate) fn market_spec_from_value(
    instrument: &InstrumentId,
    market: &serde_json::Value,
) -> Result<TradingInstrumentSpec, String> {
    let spec = if market_value_is_product_spec(market) {
        let spec: TradingInstrumentSpec =
            serde_json::from_value(market.clone()).map_err(|error| {
                format!("产品规格 market spec 反序列化失败（带 base_currency 的那种形状）: {error}")
            })?;
        if spec.instrument != *instrument {
            return Err(format!(
                "market spec 的 instrument {} 与标的 {instrument} 不一致",
                spec.instrument
            ));
        }
        spec
    } else {
        ccxt_market_to_spec(instrument, market)?
    };
    spec.validate()
        .map_err(|error| format!("market spec 字段非法: {error:?}"))?;
    Ok(spec)
}

/// CCXT 归一化 market 快照 → 产品规格。只由 [`market_spec_from_value`] 调用。
fn ccxt_market_to_spec(
    instrument: &InstrumentId,
    market: &serde_json::Value,
) -> Result<TradingInstrumentSpec, String> {
    let market_type = market
        .get("market_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("spot");
    let product = match market_type {
        "spot" => TradingProduct::Spot,
        "margin" => TradingProduct::Margin,
        "swap" | "perpetual" => TradingProduct::Perpetual,
        "future" | "futures" => TradingProduct::Future,
        other => return Err(format!("CCXT market type 不支持: {other}")),
    };
    let base_currency = market
        .get("base")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "CCXT market 缺少 base".to_string())?;
    let quote_currency = market
        .get("quote")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "CCXT market 缺少 quote".to_string())?;
    let settlement_currency = market
        .get("settle")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(quote_currency);
    let contract_size = raw_json_i128(market, "contract_size_raw")?;
    let linear = market
        .get("linear")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(!product.is_derivative());
    let inverse = market
        .get("inverse")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let max_leverage = match market
        .get("max_leverage")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
    {
        Some(0) => return Err("CCXT market max_leverage 不能为 0".into()),
        Some(value) => value,
        // 现货没有杠杆可猜；衍生品的上限一旦兜底就成了"未知产品允许 100 倍"。
        None if product.is_derivative() => {
            return Err(declared_field_problem(
                "max_leverage",
                "衍生品杠杆上限",
                "100x",
            ))
        }
        None => 1,
    };
    let maintenance_margin_bps = match market
        .get("maintenance_margin_bps")
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
    {
        Some(value) if value <= 10_000 => value,
        Some(value) => return Err(format!("CCXT market maintenance_margin_bps 非法: {value}")),
        // 同一条理由：兜一个 500bp 等于用猜出来的保证金率算强平线。
        None if product.is_derivative() => {
            return Err(declared_field_problem(
                "maintenance_margin_bps",
                "衍生品维持保证金率",
                "500bp",
            ))
        }
        None => 0,
    };
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product,
        base_currency: base_currency.into(),
        quote_currency: quote_currency.into(),
        settlement_currency: settlement_currency.into(),
        contract_size,
        linear,
        inverse,
        // 三个精度口径一律要求显式声明：它们同时是下单取整闸门、最小下单量闸门和
        // `one_tick_slippage` 的唯一一档来源，兜底的 `1`（=1e-9）能通过字段校验，
        // 于是产物里看不出这一档是猜的。
        price_tick: declared_raw_number(market, "price_tick_raw", "一档价格精度")?,
        qty_step: declared_raw_number(market, "qty_step_raw", "数量步长")?,
        min_qty: declared_raw_number(market, "min_qty_raw", "最小下单量")?,
        max_leverage,
        maintenance_margin_bps,
        valid_from: 0,
        valid_to: market.get("expiry_ms").and_then(serde_json::Value::as_u64),
    };
    Ok(spec)
}

/// CCXT 归一化快照里必须显式声明的定点口径；缺失或非正时拒绝，而不是兜一个最小单位。
fn declared_raw_number(market: &serde_json::Value, key: &str, what: &str) -> Result<i128, String> {
    let declared = raw_json_i128(market, key)
        .map_err(|_| declared_field_problem(key, what, "1 个最小报价单位"))?;
    if declared <= 0 {
        return Err(format!("CCXT market {key} 必须为正，实际 {declared}"));
    }
    Ok(declared)
}

/// 缺字段/非法字段共用的那条报错：点名缺什么、兜底值会伪装成什么、该怎么补。
fn declared_field_problem(key: &str, what: &str, fallback: &str) -> String {
    format!(
        "CCXT market 缺少 {key}（{what}）：兜底成 {fallback} 会让闸门和滑点口径静默失效，\
         而产物里没有任何地方写着它是猜的。交易所未报这一口径时请改传已冻结的产品规格 \
         market spec（带 base_currency/instrument 的那种形状）。"
    )
}

pub(crate) fn ccxt_margin_rule_from_market(market: &serde_json::Value) -> Box<dyn MarginRule> {
    let Some(rows) = market
        .get("leverage_tiers")
        .and_then(serde_json::Value::as_array)
    else {
        return Box::new(NoMargin);
    };
    let tiers = rows
        .iter()
        .filter_map(|row| {
            let max_notional = row
                .get("max_notional_raw")
                .and_then(serde_json::Value::as_i64)
                .map(i128::from)
                .or_else(|| {
                    row.get("max_notional_raw")
                        .and_then(serde_json::Value::as_u64)
                        .map(i128::from)
                })
                .unwrap_or(i128::MAX);
            let initial_bp = row
                .get("initial_margin_bps")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            let maintenance_bp = row
                .get("maintenance_margin_bps")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            let max_leverage = row
                .get("max_leverage")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok());
            (max_notional > 0 && initial_bp >= 0 && maintenance_bp >= 0).then_some(MarginTier {
                max_notional,
                initial_bp,
                maintenance_bp,
                max_leverage,
            })
        })
        .collect::<Vec<_>>();
    if tiers.is_empty() {
        Box::new(NoMargin)
    } else {
        Box::new(TieredMargin { tiers })
    }
}
