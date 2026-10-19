//! L1/L2 订单簿逐档撮合内核；模块不依赖网络或存储，盘口快照先做顺序/价格/数量校验，
//! 再按价格优先、同价位数量优先逐档消费。
//!
//! 真实消费者都在本 crate 内：L2 深度回测（`orderbook_backtest`）直接逐档撮合，L1 Tick
//! 回测（`tick_backtest`）把 bid/ask 首档折叠成快照后复用同一个引擎，两者没有第二套订单语义。
//!
//! **Paper 不走这里。** 它的成交由 `qx-zhenlu` 的 `PaperVenue::on_quote` 对每条 QuoteTick
//! 用首档一次性 touch 产生，没有逐档队列、没有排队中的部分成交，所以"Paper 共用簿内核"是不
//! 成立的说法（V11 §4.3）。paper 与回测目前真正共享的是执行平面下游的 `FeeModel`、
//! `qx_core::apply_fill_to_books` 与 `Ledger`。把 paper 改接到本内核是 V11 §9 登记的 Q1d；
//! 在那之前，涉及 paper 撮合的表述必须写明"首档 touch"。

use crate::{ExecutionAttempt, ExecutionAttemptReason, ExecutionLiquidityEvidence};
use qx_core::{Fill, Order, OrderStatus, Price, Quantity, Side, TradingInstrumentSpec, SCALE};
use serde::{Deserialize, Serialize};

mod audit;
mod level_lookup;
mod matching;
pub use audit::{OrderBookExecutionDescriptor, QueuePositionDescriptor};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BookLevel {
    pub price: Price,
    pub qty: Quantity,
}

impl BookLevel {
    pub fn validate(&self) -> Result<(), String> {
        if self.price.raw() <= 0 || self.qty.raw() <= 0 {
            return Err("盘口档位价格和数量必须为正".into());
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OrderBookSnapshot {
    pub instrument: qx_core::InstrumentId,
    pub ts: u64,
    pub sequence: u64,
    /// 买盘按价格从高到低排列。
    pub bids: Vec<BookLevel>,
    /// 卖盘按价格从低到高排列。
    pub asks: Vec<BookLevel>,
}

impl OrderBookSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.ts == 0 || self.sequence == 0 {
            return Err("OrderBookSnapshot ts/sequence 必须为正".into());
        }
        for level in self.bids.iter().chain(self.asks.iter()) {
            level.validate()?;
        }
        if self
            .bids
            .windows(2)
            .any(|window| window[0].price.raw() <= window[1].price.raw())
        {
            return Err("买盘必须严格按价格降序排列".into());
        }
        if self
            .asks
            .windows(2)
            .any(|window| window[0].price.raw() >= window[1].price.raw())
        {
            return Err("卖盘必须严格按价格升序排列".into());
        }
        if let (Some(bid), Some(ask)) = (self.bids.first(), self.asks.first()) {
            if bid.price.raw() > ask.price.raw() {
                return Err("盘口买一不能高于卖一".into());
            }
        }
        Ok(())
    }
}

/// Whether a snapshot's shape matches the declared best-bid/best-ask-only tier.
/// Keep this predicate shared by single- and multi-market backtests so a route cannot
/// silently accept L2 depth under an L1 declaration.
pub(crate) fn has_l1_shape(snapshot: &OrderBookSnapshot) -> bool {
    snapshot.bids.len() == 1 && snapshot.asks.len() == 1
}

pub(crate) fn validate_backtest_data_tier(
    data_tier: crate::DataTier,
    snapshots: &[OrderBookSnapshot],
) -> Result<(), String> {
    match data_tier {
        crate::DataTier::Bar => {
            Err("订单簿回测需要盘口快照档位（L1 或 L2/L3），不能声明为 Bar".into())
        }
        crate::DataTier::L1 if snapshots.iter().any(|snapshot| !has_l1_shape(snapshot)) => {
            Err("订单簿回测声明 L1 时每份快照必须恰有一档 bid/ask".into())
        }
        crate::DataTier::L1 | crate::DataTier::L2L3 => Ok(()),
    }
}

/// 一份可交付的 L1/L2 深度数据帧。`source` 必填，用于把回测输入绑定到
/// 数据来源；快照顺序、价格和数量由 `OrderBookSnapshot::validate` 约束。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepthFrame {
    pub schema_version: u32,
    pub source: String,
    pub instrument: qx_core::InstrumentId,
    pub snapshots: Vec<OrderBookSnapshot>,
}

impl DepthFrame {
    pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

    pub fn from_json(input: &str) -> Result<Self, String> {
        let frame: Self = serde_json::from_str(input)
            .map_err(|error| format!("深度数据帧 JSON 无效: {error}"))?;
        frame.validate()?;
        Ok(frame)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != Self::SUPPORTED_SCHEMA_VERSION {
            return Err(format!(
                "深度数据帧 schema_version 仅支持 {}，实际 {}",
                Self::SUPPORTED_SCHEMA_VERSION,
                self.schema_version
            ));
        }
        if self.source.trim().is_empty() {
            return Err("深度数据帧 source 不能为空".into());
        }
        if self.snapshots.is_empty() {
            return Err("深度数据帧至少需要一份盘口快照".into());
        }
        for snapshot in &self.snapshots {
            snapshot.validate()?;
            if snapshot.instrument != self.instrument {
                return Err(format!(
                    "深度数据帧快照标的与声明不一致: {} != {}",
                    snapshot.instrument, self.instrument
                ));
            }
        }
        Ok(())
    }

    /// 输入数据指纹；与撮合结果无关，只描述深度序列本身。
    pub fn input_hash(&self) -> u64 {
        let mut hash = qx_core::Fnv1a::new();
        hash.write_text(&self.source);
        hash.write_text(&self.instrument.to_string());
        hash.write_u64(self.snapshots.len() as u64);
        for snapshot in &self.snapshots {
            hash.write_u64(snapshot.ts);
            hash.write_u64(snapshot.sequence);
            hash.write_u64(snapshot.bids.len() as u64); // Preserve the flattened book's bid/ask boundary.
            hash.write_u64(snapshot.asks.len() as u64);
            for level in snapshot.bids.iter().chain(snapshot.asks.iter()) {
                hash.write_i128(level.price.raw());
                hash.write_i128(level.qty.raw());
            }
        }
        hash.finish()
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct PendingBookOrder {
    order: Order,
    ready_at_snapshot: u64,
    queue_state: Option<PendingQueueState>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct PendingQueueState {
    price_raw: i128,
    queue_ahead_raw: i128,
    displayed_qty_raw: i128,
}

/// Explicit L2 queue-position assumption. Depth reductions are only a proxy for traded volume:
/// cancellations and new hidden liquidity cannot be distinguished without trade prints. With
/// finite-depth snapshots, an absent limit-price level is insufficient evidence and cannot
/// establish or advance a queue baseline.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueuePositionModel {
    #[default]
    Disabled,
    FifoVolumeEstimate {
        volume_advance_bps: u32,
    },
}

impl QueuePositionModel {
    pub fn validate(self) -> Result<(), String> {
        if let Self::FifoVolumeEstimate { volume_advance_bps } = self {
            if volume_advance_bps > 10_000 {
                return Err("FIFO 队列成交量前推比例必须在 0..=10000 bp 内".into());
            }
        }
        Ok(())
    }

    pub fn descriptor(self, initial_queue_bps: i128) -> String {
        match self {
            Self::Disabled => "queue_position=disabled".into(),
            Self::FifoVolumeEstimate {
                volume_advance_bps,
            } => format!(
                "queue_position=fifo_volume_estimate@v2[initial_queue_bps={initial_queue_bps};volume_advance_bps={volume_advance_bps};assumption=displayed_depth_reduction_estimates_traded_volume;limits=hidden_liquidity_and_cancellations_are_unobserved,missing_limit_level_has_no_baseline_and_cannot_advance_queue]"
            ),
        }
    }
}

/// 订单簿回测的确定性执行假设。
///
/// `latency_snapshots` 表示订单提交后需要等待的后续快照数量；
/// `queue_position_bps` 将限价订单所在档位的一部分流动性视为前置队列；
/// `market_impact_bps` 对实际成交价施加固定比例冲击。默认模型保持原有
/// 逐档吃单行为，所有参数都应写入上层回测配置和 RunManifest。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OrderBookExecutionModel {
    pub fee_bps: i128,
    pub latency_snapshots: u64,
    pub queue_position_bps: i128,
    pub queue_position_model: QueuePositionModel,
    pub market_impact_bps: i128,
}

impl OrderBookExecutionModel {
    pub fn new(fee_bps: i128) -> Result<Self, String> {
        let model = Self {
            fee_bps,
            latency_snapshots: 0,
            queue_position_bps: 0,
            queue_position_model: QueuePositionModel::Disabled,
            market_impact_bps: 0,
        };
        model.validate()?;
        Ok(model)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(0..=10_000).contains(&self.fee_bps)
            || !(0..=10_000).contains(&self.queue_position_bps)
            || !(0..=10_000).contains(&self.market_impact_bps)
        {
            return Err("订单簿执行模型 bps 必须在 0..=10000 内".into());
        }
        self.queue_position_model.validate()?;
        Ok(())
    }
}

/// 逐档吃单撮合器。它只负责产生 Fill，不直接修改 Ledger。
pub struct OrderBookMatchingEngine {
    pending: Vec<PendingBookOrder>,
    /// Per-snapshot work buffers retain capacity across the long-lived matcher loop.
    scratch_pending: Vec<PendingBookOrder>,
    scratch_remaining_bids: Vec<i128>,
    scratch_remaining_asks: Vec<i128>,
    scratch_queue_bids: Vec<i128>,
    scratch_queue_asks: Vec<i128>,
    scratch_estimated_volume_budget: std::collections::HashMap<(bool, i128), i128>,
    execution_attempts: Vec<ExecutionAttempt>,
    model: OrderBookExecutionModel,
    snapshot_index: u64,
    last_snapshot_sequence: Option<u64>,
    last_snapshot_ts: Option<u64>,
    /// 手续费基准所用产品规格。Ledger 用 `notional(qty, price)` 记账，
    /// 费用必须走同一口径，否则 `contract_size != 1` 的合约手续费会与
    /// 真实名义额脱钩。`None` 保持现货 `qty * price` 口径。
    fee_spec: Option<TradingInstrumentSpec>,
}

impl OrderBookMatchingEngine {
    pub fn new(fee_bps: i128) -> Result<Self, String> {
        Self::with_model(OrderBookExecutionModel::new(fee_bps)?)
    }

    pub fn with_model(model: OrderBookExecutionModel) -> Result<Self, String> {
        model.validate()?;
        Ok(Self {
            pending: Vec::new(),
            scratch_pending: Vec::new(),
            scratch_remaining_bids: Vec::new(),
            scratch_remaining_asks: Vec::new(),
            scratch_queue_bids: Vec::new(),
            scratch_queue_asks: Vec::new(),
            scratch_estimated_volume_budget: std::collections::HashMap::new(),
            execution_attempts: Vec::new(),
            model,
            snapshot_index: 0,
            last_snapshot_sequence: None,
            last_snapshot_ts: None,
            fee_spec: None,
        })
    }

    /// 指定手续费基准使用的产品规格，使费用名义额与 `Ledger::apply_fill_with_spec`
    /// 记账的 `TradingInstrumentSpec::notional` 保持一致。
    pub fn with_fee_spec(mut self, spec: Option<TradingInstrumentSpec>) -> Self {
        self.fee_spec = spec;
        self
    }

    pub fn submit(&mut self, order: Order) -> Result<(), String> {
        order
            .validate()
            .map_err(|error| format!("订单簿订单非法: {error}"))?;
        if self
            .pending
            .iter()
            .any(|pending| pending.order.client_id == order.client_id)
        {
            return Err(format!("订单簿重复 client_id: {}", order.client_id));
        }
        let ready_at_snapshot = self
            .snapshot_index
            .saturating_add(self.model.latency_snapshots)
            .saturating_add(1);
        self.pending.push(PendingBookOrder {
            order,
            ready_at_snapshot,
            queue_state: None,
        });
        Ok(())
    }

    /// 在下一次快照撮合前撤销一个尚未终态的订单。
    ///
    /// 撤单是否先于盘口事件生效由调用方的事件顺序决定；回测可使用
    /// `on_snapshot_with_cancellations` 将同一时间点的撤单明确排在撮合前。
    pub fn cancel(&mut self, client_id: u64) -> bool {
        let before = self.pending.len();
        self.pending
            .retain(|pending| pending.order.client_id != client_id);
        before != self.pending.len()
    }

    pub fn cancel_many(&mut self, client_ids: &[u64]) -> usize {
        client_ids
            .iter()
            .filter(|client_id| self.cancel(**client_id))
            .count()
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn execution_attempts(&self) -> &[ExecutionAttempt] {
        &self.execution_attempts
    }

    pub fn on_snapshot(&mut self, snapshot: &OrderBookSnapshot) -> Result<Vec<Fill>, String> {
        self.on_snapshot_with_cancellations(snapshot, &[])
    }

    /// 应用一组在该盘口快照撮合前生效的撤单，再按快照撮合。

    fn record_attempt(
        &mut self,
        order: &Order,
        ts: u64,
        requested_qty_raw: i128,
        filled_qty_raw: i128,
        price_raw: Option<i128>,
        reason: ExecutionAttemptReason,
        liquidity: ExecutionLiquidityEvidence,
    ) {
        self.execution_attempts.push(ExecutionAttempt {
            ts,
            order_id: order.client_id,
            instrument: order.instrument.to_string(),
            side: order.side,
            requested_qty_raw,
            filled_qty_raw,
            remaining_qty_raw: requested_qty_raw.saturating_sub(filled_qty_raw),
            price_raw,
            reason,
            displayed_qty_raw: liquidity.displayed_qty_raw,
            queue_ahead_before_raw: liquidity.queue_ahead_before_raw,
            queue_ahead_after_raw: liquidity.queue_ahead_after_raw,
            estimated_volume_raw: liquidity.estimated_volume_raw,
            displayed_reduction_raw: liquidity.displayed_reduction_raw,
            queue_consumed_raw: liquidity.queue_consumed_raw,
        });
    }

    fn queue_ahead(&self, level_qty: i128) -> i128 {
        level_qty
            .checked_mul(self.model.queue_position_bps)
            .and_then(|value| value.checked_div(10_000))
            .unwrap_or(level_qty)
    }

    fn execution_price(&self, side: Side, raw: i128) -> Option<i128> {
        let impact = raw
            .checked_mul(self.model.market_impact_bps)
            .and_then(|value| value.checked_div(10_000))?;
        let price = match side {
            Side::Buy => raw.checked_add(impact)?,
            Side::Sell => raw.checked_sub(impact)?,
        };
        (price > 0).then_some(price)
    }

    fn limit_ok(order: &Order, market_price: Price) -> bool {
        match order.limit {
            None => true,
            Some(limit) => match order.side {
                Side::Buy => market_price.raw() <= limit.raw(),
                Side::Sell => market_price.raw() >= limit.raw(),
            },
        }
    }
}

#[cfg(test)]
#[path = "orderbook/tests.rs"]
mod tests;
