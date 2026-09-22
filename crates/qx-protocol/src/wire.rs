//! 账户状态的跨语言线格式与其到内核观察类型的唯一转换层（V10 §4.7）。
//!
//! 这里的类型全部是 `*_raw: i128` 定长整数的编码形状，字段与 `qx-core` 的
//! 领域观察类型（`AccountPositionSnapshot` / `AccountBalance` / `Order` /
//! `Fill`）**合法地不同**——线格式要跨语言、要 schema 稳定、要定长整数，因此
//! 不能做成再导出；重复的风险在于"各处手抄折算"，所以内核 ↔ 线格式的全部
//! `From` / `from_fact` / `upsert_*` 折算只允许住在这一个文件里。

use super::*;

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SnapshotHeader {
    pub schema_version: u32,
    pub snapshot_id: u64,
    pub account_id: String,
    pub portfolio_id: String,
    pub venue_id: String,
    pub trading_day: String,
    pub as_of: u64,
    pub event_seq: u64,
    pub state_hash: u64,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PositionSnapshot {
    pub instrument: InstrumentId,
    pub quantity_raw: i128,
    pub today_quantity_raw: i128,
    pub average_price_raw: i128,
    pub mark_price_raw: i128,
    /// 与账户级同名标量同一条纪律（V11 Q67/Q68）：`None` 是"交易所没报这一项"，
    /// `Some(0)` 是"报了且为零"。价格是定点数、0 不是合法价格，所以那两列继续用
    /// `0` 表达"没有"；钱没有这个性质，必须显式缺席。
    pub unrealized_pnl_raw: Option<i128>,
    pub margin_raw: Option<i128>,
}

impl Default for PositionSnapshot {
    fn default() -> Self {
        Self {
            instrument: InstrumentId::parse("UNKNOWN.UNKNOWN").expect("valid sentinel instrument"),
            quantity_raw: 0,
            today_quantity_raw: 0,
            average_price_raw: 0,
            mark_price_raw: 0,
            unrealized_pnl_raw: None,
            margin_raw: None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OrderSnapshot {
    pub order_id: u64,
    pub client_order_id: u64,
    pub instrument: InstrumentId,
    pub side: Side,
    pub quantity_raw: i128,
    pub filled_raw: i128,
    pub status: OrderStatus,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FillSnapshot {
    pub fill_id: u64,
    pub order_id: u64,
    pub quantity_raw: i128,
    pub price_raw: i128,
    pub fee_raw: i128,
    pub ts: u64,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TransferSnapshot {
    pub transfer_id: u64,
    pub currency: String,
    pub amount_raw: i128,
    pub ts: u64,
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct ReconcileSnapshot {
    pub last_reconcile_ts: u64,
    pub discrepancy_count: u32,
    pub recovery_state: String,
}

/// 内核持仓观察 → 线格式持仓的**唯一**折算。
///
/// 有意的线格式收敛（不是第二套定义，只是编码）：
/// - 内核不区分当日持仓，`today_quantity_raw` 与 `quantity_raw` 同值；需要当日
///   维度的调用方必须自行补齐，不得再抄一份折算。
/// - `initial_margin` / `maintenance_margin` 在线格式上合并为一个 `margin_raw`，
///   取初始保证金（与历史 `api_service` 行为一致）。
/// - 三个钱字段把内核的"没报"原样编码成 `null`，折算层不得在这里补一个 0。
impl From<&AccountPositionSnapshot> for PositionSnapshot {
    fn from(observation: &AccountPositionSnapshot) -> Self {
        Self {
            instrument: observation.instrument.clone(),
            quantity_raw: observation.quantity.raw(),
            today_quantity_raw: observation.quantity.raw(),
            average_price_raw: observation.average_price.map(Price::raw).unwrap_or(0),
            mark_price_raw: observation.mark_price.map(Price::raw).unwrap_or(0),
            unrealized_pnl_raw: observation.unrealized_pnl.map(Money::raw),
            margin_raw: observation.initial_margin.map(Money::raw),
        }
    }
}

/// 线格式持仓 → 内核持仓观察（恢复 / 对账读回路径）。
///
/// 有意的信息损失：线格式不携带强平价、杠杆、保证金模式与持仓方向，读回为
/// `None`；价格为 0 读回 `None`（线格式无法区分"未知"与"零价"，而零价非法），
/// 线格式也只有一个保证金列，`maintenance_margin` 读回 `None` 而不是 0。
impl From<&PositionSnapshot> for AccountPositionSnapshot {
    fn from(wire: &PositionSnapshot) -> Self {
        Self {
            instrument: wire.instrument.clone(),
            quantity: Quantity::from_raw(wire.quantity_raw),
            average_price: (wire.average_price_raw != 0)
                .then_some(Price::from_raw(wire.average_price_raw)),
            mark_price: (wire.mark_price_raw != 0).then_some(Price::from_raw(wire.mark_price_raw)),
            liquidation_price: None,
            unrealized_pnl: wire.unrealized_pnl_raw.map(Money::from_raw),
            initial_margin: wire.margin_raw.map(Money::from_raw),
            maintenance_margin: None,
            leverage: None,
            margin_mode: None,
            position_side: None,
        }
    }
}

/// 内核订单 → 线格式订单的**唯一**折算。
///
/// 线格式不携带限价/账户/追踪信息，只做查询投影；`order_id` 与
/// `client_order_id` 同值（EventLog 恢复路径没有独立的 venue order id）。
impl From<&Order> for OrderSnapshot {
    fn from(order: &Order) -> Self {
        Self {
            order_id: order.client_id,
            client_order_id: order.client_id,
            instrument: order.instrument.clone(),
            side: order.side,
            quantity_raw: order.qty.raw(),
            filled_raw: order.filled.raw(),
            status: order.status,
        }
    }
}

impl FillSnapshot {
    /// 内核成交事实 → 线格式成交的**唯一**折算。
    ///
    /// `fill_id` 来自事件序号而不是成交本身（`qx_core::Fill` 没有自带 id），
    /// 因此这里显式要求调用方传入，避免各处自行决定 id 语义。
    pub fn from_fact(fill_id: u64, fill: &Fill) -> Self {
        Self {
            fill_id,
            order_id: fill.order_id,
            quantity_raw: fill.qty.raw(),
            price_raw: fill.price.raw(),
            fee_raw: fill.fee.raw(),
            ts: fill.ts,
        }
    }
}

impl AccountSnapshot {
    /// 内核余额观察 → 线格式 `cash_raw` 的**唯一**折算：净现金一律走
    /// [`AccountBalance::net_cash_raw`]（`free + locked - borrowed`）。
    ///
    /// 溢出时 fail-closed 返回 `Err`，不静默截断。
    pub fn upsert_balance(&mut self, balance: &AccountBalance) -> Result<(), ProtocolError> {
        let raw = balance
            .net_cash_raw()
            .ok_or_else(|| ProtocolError::Invalid("账户余额净现金折算溢出".into()))?;
        self.cash_raw.insert(balance.asset.clone(), raw);
        Ok(())
    }

    /// 批量写入余额观察；任何一条溢出都不部分生效。
    pub fn upsert_balances(&mut self, balances: &[AccountBalance]) -> Result<(), ProtocolError> {
        let staged: Vec<(String, i128)> = balances
            .iter()
            .map(|balance| {
                balance
                    .net_cash_raw()
                    .map(|raw| (balance.asset.clone(), raw))
                    .ok_or_else(|| ProtocolError::Invalid("账户余额净现金折算溢出".into()))
            })
            .collect::<Result<Vec<_>, ProtocolError>>()?;
        for (asset, raw) in staged {
            self.cash_raw.insert(asset, raw);
        }
        Ok(())
    }
}
