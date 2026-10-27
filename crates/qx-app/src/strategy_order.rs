//! Strategy output domain rules shared by CLI, HTTP and native callers.

use crate::{AppError, AppErrorCategory};
use qx_core::{
    ExactDecimal, ExactOrderAmounts, Fnv1a, InstrumentId, MarginMode, Order, OrderPolicy,
    OrderStatus, PositionMode, PositionSide, Price, Quantity, Side, TradingInstrumentSpec,
    TradingProduct,
};
use qx_zhenlu::{rebalance_intent, Signal, SignalMerger};

/// Runtime-independent bindings for converting a strategy target position into an OMS order.
/// The facade resolves values from runtime configuration and passes the frozen market spec.
pub struct StrategyTargetOrderInput<'a> {
    pub instrument: &'a str,
    pub account_id: &'a str,
    pub strategy_id: &'a str,
    pub run_id: u64,
    pub now: u64,
    pub current_qty: i128,
    pub target_qty: i128,
    pub current_qty_exact: Option<&'a str>,
    pub target_qty_exact: Option<&'a str>,
    pub signal_id: u64,
    pub confidence: i128,
    pub priority: i32,
    pub expires_at: u64,
    pub product: Option<TradingProduct>,
    pub position_mode: Option<PositionMode>,
    pub margin_mode: MarginMode,
    pub leverage: Option<u32>,
    pub has_explicit_policy: bool,
    pub market_spec: Option<&'a TradingInstrumentSpec>,
}

/// Rebalance one strategy target and project it to the shared OMS order shape.
/// Risk evaluation remains with the caller because its rules are runtime policy.
pub fn build_strategy_target_order(
    input: &StrategyTargetOrderInput<'_>,
) -> Result<Option<Order>, AppError> {
    let instrument = InstrumentId::parse(input.instrument).ok_or_else(|| {
        invalid_strategy_order(format!("Strategy instrument 非法: {}", input.instrument))
    })?;
    if let Some(target_text) = input.target_qty_exact {
        if input.target_qty != 0 {
            return Err(invalid_strategy_order(
                "提供 target_qty_exact 时 target_qty 必须为 0",
            ));
        }
        if input.position_mode == Some(PositionMode::Hedge) {
            return Err(invalid_strategy_order(
                "Hedge 模式下 target_qty_exact 有歧义，请使用带 position_side 的 long/short intents",
            ));
        }
        let current_text = input.current_qty_exact.ok_or_else(|| {
            invalid_strategy_order("target_qty_exact 缺少账簿提供的 current_qty_exact")
        })?;
        let current_exact = ExactDecimal::parse(current_text)
            .map_err(|error| invalid_strategy_order(format!("current_qty_exact 非法: {error}")))?;
        let target_exact = ExactDecimal::parse(target_text)
            .map_err(|error| invalid_strategy_order(format!("target_qty_exact 非法: {error}")))?;
        let delta = target_exact
            .checked_sub(current_exact)
            .ok_or_else(|| invalid_strategy_order("exact target 与 current quantity 算术溢出"))?;
        if delta.is_zero() {
            return Ok(None);
        }
        let quantity = if delta.is_negative() {
            delta
                .checked_neg()
                .ok_or_else(|| invalid_strategy_order("exact target delta 取绝对值溢出"))?
        } else {
            delta
        };
        let round_scope = format!("strategy-target:{}", input.run_id.max(1));
        let built = build_strategy_order_from_intent(
            &StrategyOrderInput {
                intent_id: input.signal_id.max(1),
                instrument: input.instrument.to_string(),
                side: if delta.is_positive() { "buy" } else { "sell" }.into(),
                qty_raw: 0,
                qty_exact: Some(quantity.to_string()),
                quote_budget_exact: None,
                limit_price_raw: None,
                limit_price_exact: None,
                reduce_only: false,
                post_only: false,
                position_side: None,
                margin_mode: None,
                position_mode: Some("one_way".into()),
                leverage: input.leverage,
            },
            &StrategyOrderBindings {
                account_id: input.account_id,
                strategy_id: input.strategy_id,
                signal_id: input.signal_id,
                round_scope: &round_scope,
                rule_version: input.strategy_id,
                product: input.product,
                position_mode: Some(PositionMode::OneWay),
                margin_mode: Some(input.margin_mode),
                leverage: input.leverage,
                market_spec: input.market_spec,
            },
        )?;
        return Ok(Some(built.order));
    }
    let current = qx_zhenlu::portfolio::PortfolioState {
        portfolio_id: input.strategy_id.into(),
        timestamp: input.now,
        cash: 0,
        positions: std::collections::BTreeMap::from([(
            input.instrument.to_string(),
            input.current_qty,
        )]),
    };
    let rebalance = qx_zhenlu::portfolio::rebalance(
        &current,
        &[qx_zhenlu::portfolio::TargetPosition::single(
            instrument.clone(),
            input.target_qty,
        )],
        &qx_zhenlu::portfolio::PortfolioConstraint {
            max_turnover_bps: 10_000,
            min_trade_size: 1,
        },
    )
    .map_err(|error| invalid_strategy_order(error.to_string()))?;
    if rebalance.positions.is_empty() {
        return Ok(None);
    }
    if input.account_id.trim().is_empty() {
        return Err(invalid_strategy_order(
            "Strategy 产生订单必须配置 account_id",
        ));
    }
    let signal = Signal {
        strategy_id: input.strategy_id.into(),
        signal_id: input.signal_id,
        instrument,
        target_qty: input.target_qty,
        confidence: input.confidence,
        priority: input.priority,
        expires_at: input.expires_at,
    };
    let targets = SignalMerger
        .merge(vec![signal], input.now)
        .map_err(|error| invalid_strategy_order(error.to_string()))?;
    let target = targets
        .first()
        .ok_or_else(|| invalid_strategy_order("Strategy Signal 已过期或为空"))?;
    let Some(intent) = rebalance_intent(
        target,
        input.current_qty,
        input.strategy_id,
        input.account_id,
        input.run_id.max(1),
        input.now,
    ) else {
        return Ok(None);
    };
    let mut order = intent.into_order();
    order.qty = Quantity::from_raw(normalize_strategy_order_quantity(
        order.qty.raw(),
        input.market_spec,
    )?);
    let product = input.product.unwrap_or(TradingProduct::Spot);
    let position_mode = input.position_mode.unwrap_or(PositionMode::OneWay);
    if product != TradingProduct::Spot
        || input.product.is_some()
        || input.has_explicit_policy
        || input.position_mode.is_some()
        || input.leverage.is_some()
    {
        order.policy = Some(OrderPolicy {
            reduce_only: false,
            position_side: if position_mode == PositionMode::OneWay {
                PositionSide::Net
            } else if input.target_qty >= 0 {
                PositionSide::Long
            } else {
                PositionSide::Short
            },
            margin_mode: input.margin_mode,
            position_mode,
            leverage: input.leverage.unwrap_or(1),
            post_only: false,
        });
    }
    order.validate().map_err(|error| {
        invalid_strategy_order(format!("Strategy OrderIntent 转订单失败: {error}"))
    })?;
    Ok(Some(order))
}

/// Runtime-independent projection of one validated strategy contract intent.
pub struct StrategyOrderInput {
    pub intent_id: u64,
    pub instrument: String,
    pub side: String,
    pub qty_raw: i128,
    pub qty_exact: Option<String>,
    pub quote_budget_exact: Option<String>,
    pub limit_price_raw: Option<i128>,
    pub limit_price_exact: Option<String>,
    pub reduce_only: bool,
    pub post_only: bool,
    pub position_side: Option<String>,
    pub margin_mode: Option<String>,
    pub position_mode: Option<String>,
    pub leverage: Option<u32>,
}

/// Resolved identity and runtime defaults needed to turn an intent into a core order.
/// The facade resolves these values from runtime configuration and market-spec sources.
pub struct StrategyOrderBindings<'a> {
    pub account_id: &'a str,
    pub strategy_id: &'a str,
    pub signal_id: u64,
    pub round_scope: &'a str,
    pub rule_version: &'a str,
    pub product: Option<TradingProduct>,
    pub position_mode: Option<PositionMode>,
    pub margin_mode: Option<MarginMode>,
    pub leverage: Option<u32>,
    pub market_spec: Option<&'a TradingInstrumentSpec>,
}

/// Constructed order plus the resolved margin mode used to compose the caller's RiskGate.
pub struct StrategyOrderBuild {
    pub order: Order,
    pub margin_mode: MarginMode,
}

/// Translate a validated strategy intent into the shared OMS order shape.
pub fn build_strategy_order_from_intent(
    intent: &StrategyOrderInput,
    bindings: &StrategyOrderBindings<'_>,
) -> Result<StrategyOrderBuild, AppError> {
    if bindings.account_id.trim().is_empty() {
        return Err(invalid_strategy_order(
            "Strategy 产生订单必须配置 account_id",
        ));
    }
    let instrument = InstrumentId::parse(&intent.instrument).ok_or_else(|| {
        invalid_strategy_order(format!(
            "Strategy intent instrument 非法: {}",
            intent.instrument
        ))
    })?;
    let side = match intent.side.to_ascii_lowercase().as_str() {
        "buy" => Side::Buy,
        "sell" => Side::Sell,
        other => {
            return Err(invalid_strategy_order(format!(
                "Strategy intent side 非法: {other}"
            )));
        }
    };
    let product = bindings.product.unwrap_or(TradingProduct::Spot);
    let position_mode = match intent.position_mode.as_deref() {
        None => bindings.position_mode.unwrap_or(PositionMode::OneWay),
        Some("one_way") => PositionMode::OneWay,
        Some("hedge") => PositionMode::Hedge,
        Some(other) => {
            return Err(invalid_strategy_order(format!(
                "Strategy intent position_mode 非法: {other}"
            )));
        }
    };
    let margin_mode = match intent.margin_mode.as_deref() {
        None => bindings
            .margin_mode
            .unwrap_or(if product == TradingProduct::Spot {
                MarginMode::Cash
            } else {
                MarginMode::Cross
            }),
        Some("cash") => MarginMode::Cash,
        Some("cross") => MarginMode::Cross,
        Some("isolated") => MarginMode::Isolated,
        Some(other) => {
            return Err(invalid_strategy_order(format!(
                "Strategy intent margin_mode 非法: {other}"
            )));
        }
    };
    let leverage = intent.leverage.unwrap_or(bindings.leverage.unwrap_or(1));
    let position_side = match intent
        .position_side
        .as_deref()
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("long") => PositionSide::Long,
        Some("short") => PositionSide::Short,
        Some("net") | None if position_mode == PositionMode::OneWay => PositionSide::Net,
        Some("net") | None => {
            if side == Side::Buy {
                PositionSide::Long
            } else {
                PositionSide::Short
            }
        }
        Some(other) => {
            return Err(invalid_strategy_order(format!(
                "Strategy intent position_side 非法: {other}"
            )));
        }
    };
    let policy = OrderPolicy {
        reduce_only: intent.reduce_only,
        position_side,
        margin_mode,
        position_mode,
        leverage,
        post_only: intent.post_only,
    };
    let (qty, limit, exact_amounts) = if let Some(budget_text) =
        intent.quote_budget_exact.as_deref()
    {
        if intent.qty_raw != 0
            || intent.qty_exact.is_some()
            || intent.limit_price_raw.is_some()
            || intent.limit_price_exact.is_some()
            || side != Side::Buy
        {
            return Err(invalid_strategy_order(
                "quote_budget_exact 只能单独用于精确市价买单",
            ));
        }
        if instrument.venue.as_str() != "BITGET" || product != TradingProduct::Spot {
            return Err(invalid_strategy_order(
                "quote_budget_exact 当前只支持 Bitget 现货市价买单",
            ));
        }
        let quote_budget = ExactDecimal::parse(budget_text).map_err(|error| {
            invalid_strategy_order(format!("Strategy intent quote_budget_exact 非法: {error}"))
        })?;
        if !quote_budget.is_positive() {
            return Err(invalid_strategy_order(
                "Strategy intent quote_budget_exact 必须为正数",
            ));
        }
        let spec = bindings.market_spec.ok_or_else(|| {
            invalid_strategy_order("quote budget Strategy intent 缺少冻结 TradingInstrumentSpec")
        })?;
        if spec.instrument != instrument || spec.product != TradingProduct::Spot {
            return Err(invalid_strategy_order(
                "quote budget Strategy intent 的冻结规格必须匹配 Bitget 现货标的",
            ));
        }
        spec.validate_exact_quote_budget(quote_budget)
            .map_err(|error| invalid_strategy_order(error.to_string()))?;
        (
            Quantity::ZERO,
            None,
            Some(ExactOrderAmounts {
                quantity: ExactDecimal::ZERO,
                limit_price: None,
                filled: ExactDecimal::ZERO,
                quote_budget: Some(quote_budget),
                quote_filled: ExactDecimal::ZERO,
                market_reference_price: None,
            }),
        )
    } else if let Some(quantity_text) = intent.qty_exact.as_deref() {
        if intent.qty_raw != 0 || intent.limit_price_raw.is_some() {
            return Err(invalid_strategy_order(
                "精确 Strategy intent 必须将 qty_raw 设为 0，且不得同时提供 limit_price_raw",
            ));
        }
        let quantity = ExactDecimal::parse(quantity_text).map_err(|error| {
            invalid_strategy_order(format!("Strategy intent qty_exact 非法: {error}"))
        })?;
        let limit_price = intent
            .limit_price_exact
            .as_deref()
            .map(ExactDecimal::parse)
            .transpose()
            .map_err(|error| {
                invalid_strategy_order(format!("Strategy intent limit_price_exact 非法: {error}"))
            })?;
        let spec = bindings.market_spec.ok_or_else(|| {
            invalid_strategy_order("精确 Strategy intent 缺少冻结 TradingInstrumentSpec")
        })?;
        spec.validate_exact_order_precision(quantity, limit_price)
            .map_err(|error| invalid_strategy_order(error.to_string()))?;
        (
            Quantity::ZERO,
            None,
            Some(ExactOrderAmounts {
                quantity,
                limit_price,
                filled: ExactDecimal::ZERO,
                market_reference_price: None,
                quote_budget: None,
                quote_filled: ExactDecimal::ZERO,
            }),
        )
    } else {
        if intent.limit_price_exact.is_some() {
            return Err(invalid_strategy_order(
                "limit_price_exact 只能与 qty_exact 一起使用",
            ));
        }
        let qty_raw = normalize_strategy_order_quantity(intent.qty_raw, bindings.market_spec)?;
        (
            Quantity::from_raw(qty_raw),
            intent.limit_price_raw.map(Price::from_raw),
            None,
        )
    };
    let mut order = Order {
        client_id: strategy_order_identity(bindings.round_scope, intent.intent_id),
        instrument,
        side,
        qty,
        limit,
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: bindings.account_id.into(),
        trace: Some(qx_core::OrderTrace {
            strategy_id: Some(bindings.strategy_id.into()),
            signal_id: Some(bindings.signal_id),
            intent_id: Some(intent.intent_id),
            rule_version: Some(bindings.rule_version.into()),
        }),
        policy: None,
        exact_amounts,
    };
    if bindings.product.is_some()
        || bindings.margin_mode.is_some()
        || bindings.position_mode.is_some()
        || bindings.leverage.is_some()
        || intent.margin_mode.is_some()
        || intent.position_mode.is_some()
        || intent.leverage.is_some()
        || intent.reduce_only
        || intent.post_only
        || intent.position_side.is_some()
    {
        order.policy = Some(policy);
    }
    order.validate().map_err(|error| {
        invalid_strategy_order(format!("Strategy API v1 OrderIntent 转订单失败: {error}"))
    })?;
    Ok(StrategyOrderBuild { order, margin_mode })
}

/// Align a positive strategy quantity to the resolved market step and enforce its minimum.
///
/// Runtime configuration parsing and market-spec file loading stay in the facade; the quantity
/// rule itself belongs to the application boundary so worker and backtest adapters do not fork it.
pub fn normalize_strategy_order_quantity(
    quantity_raw: i128,
    market_spec: Option<&TradingInstrumentSpec>,
) -> Result<i128, AppError> {
    let Some(spec) = market_spec else {
        return Ok(quantity_raw);
    };
    let step = spec.qty_step;
    if step <= 0 || quantity_raw <= 0 {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "Strategy intent 数量或 market spec 数量步长非法",
        ));
    }
    let stepped = (quantity_raw / step) * step;
    if stepped < spec.min_qty {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!(
                "Strategy intent 数量 {} 按 market spec 步长 {} 向下对齐后小于最小数量 {}",
                quantity_raw, step, spec.min_qty
            ),
        ));
    }
    Ok(stepped)
}

fn strategy_order_identity(round_scope: &str, intent_id: u64) -> u64 {
    let mut hash = Fnv1a::new();
    hash.write_text(round_scope);
    hash.write_u64(intent_id);
    hash.finish().max(1)
}

fn invalid_strategy_order(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
