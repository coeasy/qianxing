//! 订单与成交。
//!
//! 状态机区分"提交意图"与"venue 事实"：
//! `Submitted` 只是离开本地的意图，`Accepted` 才代表获得 venue 确认。

use crate::clock::Ts;
use crate::identity::InstrumentId;
use crate::numeric::{ExactDecimal, Money, Price, Quantity};
use crate::trading::OrderPolicy;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum OrderStatus {
    PendingSubmit,
    Submitted,
    Accepted,
    Working,
    PartiallyFilled,
    Filled,
    CancelPending,
    Cancelled,
    Rejected,
    Expired,
    /// 连接中断或超时后的未知状态，必须进入对账，不能自动补单。
    Unknown,
}

impl OrderStatus {
    /// 是否为终态（不可再迁移）。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            OrderStatus::Filled
                | OrderStatus::Cancelled
                | OrderStatus::Rejected
                | OrderStatus::Expired
        )
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        use OrderStatus::*;
        if self == next {
            return !self.is_terminal();
        }
        matches!(
            (self, next),
            (PendingSubmit, Submitted | Rejected | Unknown)
                | (Submitted, Accepted | CancelPending | Rejected | Unknown)
                | (Accepted, Working | CancelPending | Rejected | Unknown)
                | (
                    Working,
                    PartiallyFilled | Filled | CancelPending | Rejected | Expired | Unknown
                )
                | (
                    PartiallyFilled,
                    PartiallyFilled | Filled | CancelPending | Expired | Unknown
                )
                | (CancelPending, Cancelled | Filled | Unknown)
                | (
                    Unknown,
                    Accepted | Working | PartiallyFilled | Filled | Cancelled | Rejected | Expired
                )
        )
    }

    pub fn transition(&mut self, next: Self) -> Result<(), String> {
        if self.can_transition_to(next) {
            *self = next;
            Ok(())
        } else {
            Err(format!("非法订单状态迁移: {:?} -> {:?}", self, next))
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderTrace {
    pub strategy_id: Option<String>,
    pub signal_id: Option<u64>,
    pub intent_id: Option<u64>,
    pub rule_version: Option<String>,
}

/// Precise order amounts for versioned live/crypto paths.
///
/// Legacy `qty`/`limit`/`filled` stay at scale 1e9. When this payload is present those
/// legacy amount fields must be zero/None; consumers must use this scale-bearing payload.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactOrderAmounts {
    /// Base amount for ordinary orders. A quote-budget market buy uses zero here and
    /// carries `quote_budget` instead; fills remain base quantities in `filled`.
    pub quantity: ExactDecimal,
    pub limit_price: Option<ExactDecimal>,
    pub filled: ExactDecimal,
    /// Maximum quote-currency spend for a market buy. The order's quote currency is taken
    /// from its frozen TradingInstrumentSpec. This is a distinct sizing unit, never a
    /// price-derived estimate of base quantity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_budget: Option<ExactDecimal>,
    /// Cumulative quote-currency amount consumed by fills for a quote-budget order.
    #[serde(default)]
    pub quote_filled: ExactDecimal,
    /// Fresh exact ticker price used only for risk/reservation on market orders.
    /// It is never forwarded as the venue order price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_reference_price: Option<ExactDecimal>,
}

/// Precise exchange fill values, kept separate from the legacy 1e9 amount fields.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ExactFillAmounts {
    pub quantity: ExactDecimal,
    pub price: ExactDecimal,
    /// Actual quote-currency cost reported by the venue when available. Used by quote-budget
    /// market buys and to book exact spot cash without recomputing rounded products.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_cost: Option<ExactDecimal>,
    /// The venue/simulator has completed this quote-budget order, even when lot-step rounding
    /// leaves a small unspent quote balance. This is an order lifecycle fact, not extra spend.
    #[serde(default, skip_serializing_if = "is_false")]
    pub quote_budget_complete: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Order {
    pub client_id: u64,
    pub instrument: InstrumentId,
    pub side: Side,
    pub qty: Quantity,
    /// None 表示市价单。
    pub limit: Option<Price>,
    pub status: OrderStatus,
    pub filled: Quantity,
    pub account_id: String,
    pub trace: Option<OrderTrace>,
    /// 现货/保证金/永续/期货的执行策略；None 保持现货 Cash/1x 兼容语义。
    #[serde(default)]
    pub policy: Option<OrderPolicy>,
    /// Versioned high-precision amount payload. Omitted on all legacy 1e9 orders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_amounts: Option<ExactOrderAmounts>,
}

impl Order {
    /// Compare immutable submission facts while ignoring the lifecycle state and cumulative
    /// fills, which may have advanced since the original submission was persisted.
    /// A reused client order ID is idempotent only when every economic and routing input
    /// matches the original request.
    pub fn same_submission_identity(&self, other: &Self) -> bool {
        let same_exact_request = match (self.exact_amounts, other.exact_amounts) {
            (Some(left), Some(right)) => {
                left.quantity == right.quantity
                    && left.limit_price == right.limit_price
                    && left.quote_budget == right.quote_budget
                    && left.market_reference_price == right.market_reference_price
            }
            (None, None) => true,
            _ => false,
        };

        self.client_id == other.client_id
            && self.instrument == other.instrument
            && self.side == other.side
            && self.qty == other.qty
            && self.limit == other.limit
            && self.account_id == other.account_id
            && self.trace == other.trace
            && self.policy == other.policy
            && same_exact_request
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.client_id == 0 {
            return Err("client_order_id 必须为正".into());
        }
        if let Some(exact) = self.exact_amounts {
            if self.qty.raw() != 0 || self.filled.raw() != 0 || self.limit.is_some() {
                return Err("精确订单必须清空旧 1e9 数量/价格字段，不能同时携带两套金额".into());
            }
            if let Some(quote_budget) = exact.quote_budget {
                if self.side != Side::Buy
                    || exact.limit_price.is_some()
                    || !exact.quantity.is_zero()
                    || !quote_budget.is_positive()
                {
                    return Err(
                        "计价币预算订单必须是正预算的精确市价买单，且不能携带基础币数量或限价"
                            .into(),
                    );
                }
                if exact.filled.is_negative()
                    || exact.quote_filled.is_negative()
                    || exact
                        .quote_filled
                        .checked_cmp(quote_budget)
                        .is_none_or(|ordering| ordering.is_gt())
                {
                    return Err("计价币预算订单成交量或花费超出有效范围".into());
                }
            } else {
                if !exact.quantity.is_positive() {
                    return Err("订单数量必须为正".into());
                }
                if exact.filled.is_negative()
                    || exact
                        .filled
                        .checked_cmp(exact.quantity)
                        .is_none_or(|ordering| ordering.is_gt())
                {
                    return Err("订单已成交数量超出有效范围".into());
                }
                if !exact.quote_filled.is_zero() {
                    return Err("基础币数量订单不能携带计价币成交花费".into());
                }
            }
            if exact.limit_price.is_some_and(|price| !price.is_positive()) {
                return Err("限价单价格必须为正".into());
            }
            if exact
                .market_reference_price
                .is_some_and(|price| !price.is_positive())
            {
                return Err("市价单风控参考价必须为正".into());
            }
            if exact.limit_price.is_some() && exact.market_reference_price.is_some() {
                return Err("精确限价单不能同时携带市价风险参考价".into());
            }
        } else {
            if self.qty.raw() <= 0 {
                return Err("订单数量必须为正".into());
            }
            if self.filled.raw() < 0 || self.filled.raw() > self.qty.raw() {
                return Err("订单已成交数量超出有效范围".into());
            }
            if self.limit.is_some_and(|price| price.raw() <= 0) {
                return Err("限价单价格必须为正".into());
            }
        }
        if self.account_id.trim().is_empty() {
            return Err("订单账户不能为空".into());
        }
        if self.policy.is_some_and(|policy| policy.leverage == 0) {
            return Err("订单杠杆必须大于 0".into());
        }
        Ok(())
    }

    /// Guard a legacy 1e9 consumer against silently reading an exact amount as zero.
    pub fn require_legacy_amounts(&self) -> Result<(), String> {
        if self.exact_amounts.is_some() {
            return Err("该路径尚未接入精确订单金额，拒绝读取 1e9 兼容字段".into());
        }
        Ok(())
    }

    pub fn remaining(&self) -> Quantity {
        Quantity::from_raw(self.qty.raw().saturating_sub(self.filled.raw()).max(0))
    }

    /// Exact quantity including a high-precision payload when present.
    pub fn quantity_decimal(&self) -> ExactDecimal {
        self.exact_amounts.map_or_else(
            || ExactDecimal::from_parts(self.qty.raw(), 9).expect("legacy scale is supported"),
            |amounts| amounts.quantity,
        )
    }

    pub fn quote_budget_decimal(&self) -> Option<ExactDecimal> {
        self.exact_amounts.and_then(|amounts| amounts.quote_budget)
    }

    pub fn quote_filled_decimal(&self) -> ExactDecimal {
        self.exact_amounts
            .map_or(ExactDecimal::ZERO, |amounts| amounts.quote_filled)
    }

    /// Remaining amount in the order's declared sizing unit. For quote-budget orders this
    /// is quote currency; consumers doing quantity/exposure math must use `remaining_decimal`.
    pub fn remaining_budget_decimal(&self) -> Option<ExactDecimal> {
        let amounts = self.exact_amounts?;
        amounts.quote_budget?.checked_sub(amounts.quote_filled)
    }

    pub fn is_quote_budget_order(&self) -> bool {
        self.quote_budget_decimal().is_some()
    }

    /// Whether any amount remains in the order's own sizing unit.
    pub fn has_remaining_amount(&self) -> bool {
        if self.is_quote_budget_order() {
            return self
                .remaining_budget_decimal()
                .is_some_and(ExactDecimal::is_positive);
        }
        self.remaining_decimal()
            .is_some_and(ExactDecimal::is_positive)
    }

    pub fn filled_decimal(&self) -> ExactDecimal {
        self.exact_amounts.map_or_else(
            || ExactDecimal::from_parts(self.filled.raw(), 9).expect("legacy scale is supported"),
            |amounts| amounts.filled,
        )
    }

    pub fn limit_price_decimal(&self) -> Option<ExactDecimal> {
        self.exact_amounts.map_or_else(
            || {
                self.limit.map(|price| {
                    ExactDecimal::from_parts(price.raw(), 9).expect("legacy scale is supported")
                })
            },
            |amounts| amounts.limit_price,
        )
    }

    /// Price used to value an exact pending order. A market order carries the fresh
    /// reference captured at admission so later reservations do not fall back to 1e-9.
    pub fn market_reference_price_decimal(&self) -> Option<ExactDecimal> {
        self.exact_amounts
            .and_then(|amounts| amounts.market_reference_price)
    }

    pub fn remaining_decimal(&self) -> Option<ExactDecimal> {
        self.quantity_decimal().checked_sub(self.filled_decimal())
    }

    pub fn trace_fill(
        &self,
        fill: &mut Fill,
        venue_id: Option<&str>,
        venue_order_id: Option<&str>,
    ) {
        if fill.account_id.is_empty() {
            fill.account_id = self.account_id.clone();
        }
        fill.venue_id = venue_id.map(str::to_string);
        fill.venue_order_id = venue_order_id.map(str::to_string);
        if let Some(trace) = &self.trace {
            fill.strategy_id = trace.strategy_id.clone();
            fill.signal_id = trace.signal_id;
            fill.intent_id = trace.intent_id;
            fill.rule_version = trace.rule_version.clone();
        }
    }
}

/// 成交：不可变事实。
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct FillFee {
    pub amount: Money,
    /// Venue 币种；`None` 仅用于兼容旧 Fill，按账簿币种解释。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// Venue trade IDs which contributed this fee row. Persisted so a restored adapter can
    /// deduplicate trade-level fee queries after process restart.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_trade_ids: Vec<String>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ExactFillFee {
    pub amount: ExactDecimal,
    pub currency: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_trade_ids: Vec<String>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Fill {
    pub order_id: u64,
    pub qty: Quantity,
    pub price: Price,
    pub fee: Money,
    pub ts: Ts,
    /// 成交归属账户。真实 Venue 回报必须填充；旧模拟路径允许为空并由 OMS 补齐。
    pub account_id: String,
    pub strategy_id: Option<String>,
    pub signal_id: Option<u64>,
    pub intent_id: Option<u64>,
    pub venue_id: Option<String>,
    pub venue_order_id: Option<String>,
    pub rule_version: Option<String>,
    pub fee_currency: Option<String>,
    /// 按币种拆分的成交手续费。为空时读取旧的 `fee` / `fee_currency` 两格。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fees: Vec<FillFee>,
    /// Scale-aware per-currency fee rows for fills that exceed the legacy 1e9 amount grid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exact_fees: Vec<ExactFillFee>,
    /// Versioned high-precision quantity/price payload; absent on legacy 1e9 fills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_amounts: Option<ExactFillAmounts>,
}

impl Fill {
    /// Stable suffix for identities that must distinguish scale-aware fills from their
    /// legacy 1e-9 projections. Empty for legacy fills so existing event dedup keys remain
    /// stable across the wire migration.
    pub fn exact_identity_suffix(&self) -> String {
        let mut components = Vec::new();
        if let Some(amounts) = self.exact_amounts {
            components.push(format!(
                "amount={}:{};price={}:{}",
                amounts.quantity.coefficient(),
                amounts.quantity.scale(),
                amounts.price.coefficient(),
                amounts.price.scale(),
            ));
        }
        for fee in &self.exact_fees {
            let mut trade_ids = fee.source_trade_ids.clone();
            trade_ids.sort();
            components.push(format!(
                "fee={}:{};currency={};trades={}",
                fee.amount.coefficient(),
                fee.amount.scale(),
                fee.currency.as_deref().unwrap_or_default(),
                trade_ids
                    .iter()
                    .map(|trade_id| format!("{}:{trade_id}", trade_id.len()))
                    .collect::<Vec<_>>()
                    .join(","),
            ));
        }
        if components.is_empty() {
            return String::new();
        }
        components.sort();
        format!("|exact:{}", components.join("|"))
    }

    pub fn validate_exact_amounts(&self) -> Result<(), String> {
        if let Some(exact) = self.exact_amounts {
            if self.qty.raw() != 0 || self.price.raw() != 0 {
                return Err("精确成交必须清空旧 1e9 数量/价格字段，不能同时携带两套金额".into());
            }
            if !exact.quantity.is_positive() || !exact.price.is_positive() {
                return Err("精确成交数量和价格必须为正".into());
            }
            if exact.quote_cost.is_some_and(|cost| !cost.is_positive()) {
                return Err("精确成交计价币成本必须为正".into());
            }
            if exact.quote_budget_complete && exact.quote_cost.is_none() {
                return Err("quote_budget 完成标记必须包含精确计价币成本".into());
            }
        }
        Ok(())
    }

    /// Guard a legacy 1e9 consumer against silently reading an exact fill as zero.
    pub fn require_legacy_amounts(&self) -> Result<(), String> {
        if self.exact_amounts.is_some() {
            return Err("该路径尚未接入精确成交金额，拒绝读取 1e9 兼容字段".into());
        }
        Ok(())
    }

    pub fn quantity_decimal(&self) -> ExactDecimal {
        self.exact_amounts.map_or_else(
            || ExactDecimal::from_parts(self.qty.raw(), 9).expect("legacy scale is supported"),
            |amounts| amounts.quantity,
        )
    }

    pub fn price_decimal(&self) -> ExactDecimal {
        self.exact_amounts.map_or_else(
            || ExactDecimal::from_parts(self.price.raw(), 9).expect("legacy scale is supported"),
            |amounts| amounts.price,
        )
    }

    /// 读取新旧格式统一的费用事实，并拒绝同时写入互相矛盾的字段。
    pub fn fee_charges(&self) -> Result<Vec<FillFee>, String> {
        if !self.exact_fees.is_empty() {
            return Err("精确手续费必须由精确账簿路径读取，不能投影到 1e9 Money".into());
        }
        if self.fees.is_empty() {
            return Ok(if self.fee.raw() == 0 {
                Vec::new()
            } else {
                vec![FillFee {
                    amount: self.fee,
                    currency: self.fee_currency.clone(),
                    source_trade_ids: Vec::new(),
                }]
            });
        }
        if self.fee.raw() != 0
            && (self.fees.len() != 1
                || self.fees[0].amount != self.fee
                || self.fees[0].currency.as_deref() != self.fee_currency.as_deref())
        {
            return Err("Fill 的兼容手续费字段与 fees 明细冲突".into());
        }
        for charge in &self.fees {
            if charge.amount.raw() != 0
                && charge
                    .currency
                    .as_deref()
                    .is_none_or(|currency| currency.trim().is_empty())
            {
                return Err("非零 Fill 手续费必须声明币种".into());
            }
        }
        Ok(self.fees.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_decreases() {
        let o = Order {
            client_id: 1,
            instrument: InstrumentId::parse("X.Y").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(10),
            limit: None,
            status: OrderStatus::PartiallyFilled,
            filled: Quantity::from_i64(4),
            account_id: "a".into(),
            trace: None,
            policy: None,

            exact_amounts: None,
        };
        assert_eq!(o.remaining().raw(), Quantity::from_i64(6).raw());
        assert!(!o.status.is_terminal());
    }

    #[test]
    fn unknown_requires_reconcile_before_recovery() {
        assert!(OrderStatus::PendingSubmit.can_transition_to(OrderStatus::Unknown));
        assert!(OrderStatus::Submitted.can_transition_to(OrderStatus::Unknown));
        assert!(OrderStatus::Unknown.can_transition_to(OrderStatus::Accepted));
        assert!(!OrderStatus::CancelPending.can_transition_to(OrderStatus::PartiallyFilled));
        assert!(OrderStatus::CancelPending.can_transition_to(OrderStatus::Filled));
        assert!(!OrderStatus::Filled.can_transition_to(OrderStatus::Working));
    }

    #[test]
    fn invalid_order_shape_is_rejected_before_submission() {
        let mut order = Order {
            client_id: 1,
            instrument: InstrumentId::parse("X.Y").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: Some(Price::from_i64(10)),
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "a".into(),
            trace: None,
            policy: None,

            exact_amounts: None,
        };
        assert!(order.validate().is_ok());
        order.qty = Quantity::ZERO;
        assert!(order.validate().is_err());
    }

    #[test]
    fn exact_order_amounts_keep_sub_nano_values_and_round_trip() {
        let exact = ExactOrderAmounts {
            quantity: ExactDecimal::parse("0.000000000123456789").unwrap(),
            limit_price: Some(ExactDecimal::parse("0.00000001").unwrap()),
            filled: ExactDecimal::parse("0.000000000023456789").unwrap(),
            market_reference_price: None,
            quote_budget: None,
            quote_filled: Default::default(),
        };
        let order = Order {
            client_id: 2,
            instrument: InstrumentId::parse("PEPE/USDT.OKX").unwrap(),
            side: Side::Buy,
            qty: Quantity::ZERO,
            limit: None,
            status: OrderStatus::PartiallyFilled,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: Some(exact),
        };

        order.validate().unwrap();
        assert_eq!(
            order.remaining_decimal().unwrap(),
            ExactDecimal::parse("0.0000000001").unwrap()
        );
        assert_eq!(order.quantity_decimal(), exact.quantity);
        assert_eq!(order.limit_price_decimal(), exact.limit_price);

        let encoded = serde_json::to_string(&order).unwrap();
        let decoded: Order = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, order);
        assert!(encoded.contains("\"coefficient\":\"123456789\""));
    }

    #[test]
    fn exact_amounts_reject_ambiguous_legacy_fields_and_invalid_fills() {
        let mut order = Order {
            client_id: 3,
            instrument: InstrumentId::parse("SHIB/USDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: Some(ExactOrderAmounts {
                quantity: ExactDecimal::parse("1000.0000000001").unwrap(),
                limit_price: None,
                filled: ExactDecimal::ZERO,
                market_reference_price: None,
                quote_budget: None,
                quote_filled: Default::default(),
            }),
        };
        assert!(order.validate().is_err());
        order.qty = Quantity::ZERO;
        order.validate().unwrap();

        let mut fill = Fill {
            order_id: 3,
            exact_amounts: Some(ExactFillAmounts {
                quantity: ExactDecimal::ZERO,
                price: ExactDecimal::parse("0.00000001").unwrap(),
                quote_cost: None,
                quote_budget_complete: false,
            }),
            ..Fill::default()
        };
        assert!(fill.validate_exact_amounts().is_err());
        fill.exact_amounts.as_mut().unwrap().quantity =
            ExactDecimal::parse("0.0000000001").unwrap();
        fill.validate_exact_amounts().unwrap();
    }

    #[test]
    fn order_wire_rejects_unknown_fields_and_keeps_legacy_optional_fields_readable() {
        let policy = OrderPolicy {
            reduce_only: false,
            position_side: crate::PositionSide::Net,
            margin_mode: crate::MarginMode::Cross,
            position_mode: crate::PositionMode::OneWay,
            leverage: 3,
            post_only: false,
        };
        let order = Order {
            client_id: 9,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: Some(policy),
            exact_amounts: None,
        };

        let mut misspelled_policy = serde_json::to_value(&order).unwrap();
        let object = misspelled_policy.as_object_mut().unwrap();
        let policy = object.remove("policy").unwrap();
        object.insert("polciy".into(), policy);
        assert!(serde_json::from_value::<Order>(misspelled_policy).is_err());

        let mut misspelled_policy_field = serde_json::to_value(&order).unwrap();
        misspelled_policy_field["policy"]["post_onli"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<Order>(misspelled_policy_field).is_err());

        let mut legacy = serde_json::to_value(Order {
            policy: None,
            ..order
        })
        .unwrap();
        let object = legacy.as_object_mut().unwrap();
        object.remove("policy");
        object.remove("exact_amounts");
        let decoded: Order = serde_json::from_value(legacy).unwrap();
        assert!(decoded.policy.is_none());
        assert!(decoded.exact_amounts.is_none());
    }
}
