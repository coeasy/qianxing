//! 账户交换协议。
//!
//! `CanonicalAccountSnapshot` 是跨语言、跨 Venue 的边界对象；它用于查询、恢复、
//! 对账和桥接，不是账簿事实的替代品。核心事实仍来自 EventLog、Fill、LedgerEntry。
//!
//! ## 快照概念的单点定义（V10 §4.7 / P1a）
//!
//! "持仓/余额/订单快照"这类概念只在 `qx-core` 有一份领域定义；本 crate 只拥有
//! **跨语言线格式**（`PositionSnapshot` / `OrderSnapshot` / `FillSnapshot`，全部是
//! `*_raw: i128` 的定长整数表示），并通过下面唯一的 `From` / `from_fact` 转换层与
//! 内核观察类型对接。内核观察类型由本 crate 直接再导出，因此消费者（`qx-cli`、
//! `qx-api`）从同一个 crate 拿到同一套名字，不再需要别名导入两套同名概念。

use qx_core::{Fnv1a, InstrumentId, Money, Order, OrderStatus, Price, Quantity, Side};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub use qx_core::{AccountBalance, AccountPositionSnapshot, Fill};

// 线格式与唯一转换层住在子模块（行数棘轮与概念单点都要求它独立成文件）。
mod wire;
pub use wire::*;

static SNAPSHOT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub const ACCOUNT_SNAPSHOT_JSON_SCHEMA: &str = r#"{
  "$schema":"https://json-schema.org/draft/2020-12/schema",
  "$id":"https://qianxing.dev/schema/account-snapshot-v1.json",
  "type":"object",
  "required":["protocol","schema_version","header","cash_raw","positions","orders","fills","transfers","reconcile"],
  "properties":{
    "protocol":{"const":"QIANXING_ACCOUNT"},
    "schema_version":{"const":1},
    "header":{"type":"object","required":["snapshot_id","account_id","portfolio_id","venue_id","as_of","event_seq","state_hash"]},
    "cash_raw":{"type":"object","additionalProperties":{"type":"integer"}},
    "positions":{"type":"object"},
    "orders":{"type":"object"},
    "fills":{"type":"object"},
    "transfers":{"type":"object"},
    "reconcile":{"type":"object"}
  }
}"#;

pub const PROJECTION_ENVELOPE_SCHEMA_VERSION: u32 = 1;

/// 所有查询、可视化和跨语言读模型共用的外层协议。数据内容可以是账户
/// 快照、事件批次、回测报告或因子报告，但游标、事实序号和状态哈希语义
/// 必须保持一致，客户端不能把 transport cursor 当成 EventLog seq。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ProjectionEnvelope<T> {
    pub schema_version: u32,
    pub kind: String,
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub account_id: String,
    #[serde(default)]
    pub portfolio_id: String,
    #[serde(default)]
    pub venue_id: String,
    pub as_of: u64,
    pub event_seq: u64,
    pub cursor: String,
    pub state_hash: u64,
    pub source: String,
    pub lineage: ProjectionLineage,
    pub data: T,
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct ProjectionLineage {
    #[serde(default)]
    pub dataset_version: String,
    #[serde(default)]
    pub manifest_digest: String,
    #[serde(default)]
    pub source_digest: String,
}

impl<T> ProjectionEnvelope<T> {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != PROJECTION_ENVELOPE_SCHEMA_VERSION
            || self.kind.trim().is_empty()
            || self.source.trim().is_empty()
            || self.cursor.trim().is_empty()
            || self.tenant_id.trim().is_empty()
            || self.run_id.trim().is_empty()
            || self.account_id.trim().is_empty()
            || self.portfolio_id.trim().is_empty()
            || self.venue_id.trim().is_empty()
        {
            return Err("ProjectionEnvelope schema、身份、source 或 cursor 非法".into());
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AccountSnapshot {
    pub header: SnapshotHeader,
    pub cash_raw: BTreeMap<String, i128>,
    /// 权益与下面这七个钱字段走同一条纪律（V11 Q67/Q68/Q70）：`None` 是"这一层算不出它"，
    /// `Some(0)` 是"算过且为零"。此前它是不可区分的 `i128`，于是账户持有一条没有标记价的仓位时，
    /// 读模型退回纯现金——把"算不出"印成一个看起来完全合法的权益。
    pub equity_raw: Option<i128>,
    /// 这七个钱字段是"读过才算得出"的量：`None` 表示这一层没有算它，`Some(0)` 表示算过且结果为零。
    /// 它们此前是不可区分的 `i128`，于是全仓没有写入点的 `margin_raw`/`fees_raw` 等会以"0"的身份被
    /// `GET /account/balances` 长期发布，读侧把"没算"当成"没有费用/没有保证金"。
    pub available_raw: Option<i128>,
    pub margin_raw: Option<i128>,
    pub frozen_raw: Option<i128>,
    pub realized_pnl_raw: Option<i128>,
    pub unrealized_pnl_raw: Option<i128>,
    pub fees_raw: Option<i128>,
    pub funding_raw: Option<i128>,
    #[serde(with = "instrument_map")]
    pub positions: BTreeMap<InstrumentId, PositionSnapshot>,
    pub orders: BTreeMap<u64, OrderSnapshot>,
    pub fills: BTreeMap<u64, FillSnapshot>,
    pub transfers: BTreeMap<u64, TransferSnapshot>,
    pub reconcile: ReconcileSnapshot,
}

mod instrument_map {
    use super::*;
    use serde::de::Error;

    pub fn serialize<S>(
        map: &BTreeMap<InstrumentId, PositionSnapshot>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let wire = map
            .iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect::<BTreeMap<_, _>>();
        wire.serialize(serializer)
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<BTreeMap<InstrumentId, PositionSnapshot>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = BTreeMap::<String, PositionSnapshot>::deserialize(deserializer)?;
        wire.into_iter()
            .map(|(key, value)| {
                let instrument = InstrumentId::parse(&key)
                    .ok_or_else(|| D::Error::custom(format!("非法 instrument: {key}")))?;
                Ok((instrument, value))
            })
            .collect()
    }
}

impl AccountSnapshot {
    pub fn new(
        snapshot_id: u64,
        account_id: impl Into<String>,
        portfolio_id: impl Into<String>,
        venue_id: impl Into<String>,
        as_of: u64,
    ) -> Self {
        Self {
            header: SnapshotHeader {
                schema_version: 1,
                snapshot_id,
                account_id: account_id.into(),
                portfolio_id: portfolio_id.into(),
                venue_id: venue_id.into(),
                trading_day: String::new(),
                as_of,
                event_seq: 0,
                state_hash: 0,
            },
            cash_raw: BTreeMap::new(),
            equity_raw: None,
            available_raw: None,
            margin_raw: None,
            frozen_raw: None,
            realized_pnl_raw: None,
            unrealized_pnl_raw: None,
            fees_raw: None,
            funding_raw: None,
            positions: BTreeMap::new(),
            orders: BTreeMap::new(),
            fills: BTreeMap::new(),
            transfers: BTreeMap::new(),
            reconcile: ReconcileSnapshot::default(),
        }
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.header.schema_version == 0 || self.header.account_id.trim().is_empty() {
            return Err(ProtocolError::Invalid("快照头或 account_id 非法".into()));
        }
        if self.header.state_hash != 0 && self.header.state_hash != self.state_hash() {
            return Err(ProtocolError::StateHashMismatch);
        }
        for (key, position) in &self.positions {
            if key != &position.instrument {
                return Err(ProtocolError::Invalid(
                    "position key 与 instrument 不一致".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn seal(&mut self) {
        self.header.state_hash = self.state_hash();
    }

    /// 八个汇总钱字段的唯一取法（权益排在最前，但它同样可能算不出）。哈希与稳定 JSON 都走这一份，
    /// 两处不可能对"哪些字段算未算"各说一套。
    fn scalar_money_raw(&self) -> [Option<i128>; 8] {
        [
            self.equity_raw,
            self.available_raw,
            self.margin_raw,
            self.frozen_raw,
            self.realized_pnl_raw,
            self.unrealized_pnl_raw,
            self.fees_raw,
            self.funding_raw,
        ]
    }

    /// `None` 与 `Some(0)` 必须是两个不同的哈希：前者是"这一层没算"，后者是"算过、结果为零"。
    /// 只写 `unwrap_or_default()` 会让两者撞成同一份状态，未算也就永远改不动 state_hash。
    /// 账户标量与持仓行共用这一处写入，两处不可能各定一套"未算"的编码。
    fn write_optional_money(hasher: &mut Fnv1a, value: Option<i128>) {
        hasher.write_u64(u64::from(value.is_some()));
        hasher.write_i128(value.unwrap_or_default());
    }

    fn write_scalar_money(hasher: &mut Fnv1a, values: [Option<i128>; 8]) {
        for value in values {
            Self::write_optional_money(hasher, value);
        }
    }

    /// "未算"在稳定 JSON 里只有一个字面量：`null`。持仓行与账户标量都经由这一处。
    fn money_json(value: Option<i128>) -> String {
        match value {
            Some(raw) => raw.to_string(),
            None => "null".to_string(),
        }
    }

    fn scalar_json_values(&self) -> Vec<String> {
        self.scalar_money_raw()
            .into_iter()
            .map(Self::money_json)
            .collect()
    }

    pub fn state_hash(&self) -> u64 {
        let mut h = Fnv1a::new();
        h.write_u64(self.header.schema_version as u64);
        h.write_u64(self.header.snapshot_id);
        h.write_text(&self.header.account_id);
        h.write_text(&self.header.portfolio_id);
        h.write_text(&self.header.venue_id);
        h.write_text(&self.header.trading_day);
        h.write_u64(self.header.as_of);
        h.write_u64(self.header.event_seq);
        for (currency, amount) in &self.cash_raw {
            h.write_text(currency);
            h.write_i128(*amount);
        }
        Self::write_scalar_money(&mut h, self.scalar_money_raw());
        for (instrument, position) in &self.positions {
            h.write_text(&format!("{}", instrument));
            for value in [
                position.quantity_raw,
                position.today_quantity_raw,
                position.average_price_raw,
                position.mark_price_raw,
            ] {
                h.write_i128(value);
            }
            for value in [position.unrealized_pnl_raw, position.margin_raw] {
                Self::write_optional_money(&mut h, value);
            }
        }
        for (id, order) in &self.orders {
            h.write_u64(*id);
            h.write_u64(order.client_order_id);
            h.write_text(&format!("{}", order.instrument));
            h.write_u64(side_code(order.side));
            h.write_i128(order.quantity_raw);
            h.write_i128(order.filled_raw);
            h.write_u64(order_status_code(order.status));
        }
        for (id, fill) in &self.fills {
            h.write_u64(*id);
            h.write_u64(fill.order_id);
            h.write_i128(fill.quantity_raw);
            h.write_i128(fill.price_raw);
            h.write_i128(fill.fee_raw);
            h.write_u64(fill.ts);
        }
        for (id, transfer) in &self.transfers {
            h.write_u64(*id);
            h.write_text(&transfer.currency);
            h.write_i128(transfer.amount_raw);
            h.write_u64(transfer.ts);
        }
        h.write_u64(self.reconcile.last_reconcile_ts);
        h.write_u64(self.reconcile.discrepancy_count as u64);
        h.write_text(&self.reconcile.recovery_state);
        h.finish()
    }

    pub fn diff(&self, target: &Self) -> Result<SnapshotDiff, ProtocolError> {
        if self.header.account_id != target.header.account_id
            || self.header.venue_id != target.header.venue_id
        {
            return Err(ProtocolError::IdentityMismatch);
        }
        Ok(SnapshotDiff {
            schema_version: self.header.schema_version,
            base_state_hash: self.state_hash(),
            target_state_hash: target.state_hash(),
            target_header: target.header.clone(),
            cash: diff_map(&self.cash_raw, &target.cash_raw),
            positions: diff_map(&self.positions, &target.positions),
            orders: diff_map(&self.orders, &target.orders),
            fills: diff_map(&self.fills, &target.fills),
            transfers: diff_map(&self.transfers, &target.transfers),
            replacement: if self.scalar_hash() == target.scalar_hash() {
                None
            } else {
                Some(ScalarState {
                    equity_raw: target.equity_raw,
                    available_raw: target.available_raw,
                    margin_raw: target.margin_raw,
                    frozen_raw: target.frozen_raw,
                    realized_pnl_raw: target.realized_pnl_raw,
                    unrealized_pnl_raw: target.unrealized_pnl_raw,
                    fees_raw: target.fees_raw,
                    funding_raw: target.funding_raw,
                    reconcile: target.reconcile.clone(),
                })
            },
        })
    }

    fn scalar_hash(&self) -> u64 {
        let mut h = Fnv1a::new();
        Self::write_scalar_money(&mut h, self.scalar_money_raw());
        h.write_u64(self.reconcile.last_reconcile_ts);
        h.write_u64(self.reconcile.discrepancy_count as u64);
        h.write_text(&self.reconcile.recovery_state);
        h.finish()
    }

    pub fn to_qifi(&self) -> QifiEnvelope {
        QifiEnvelope {
            protocol: "QIFI".into(),
            version: format!("v{}", self.header.schema_version),
            snapshot: self.clone(),
        }
    }

    /// 稳定 JSON 线格式：字段顺序固定、定点数传 raw integer，避免跨语言浮点漂移。
    pub fn to_json(&self) -> String {
        let positions = self
            .positions
            .iter()
            .map(|(instrument, value)| {
                format!(
                    "{}:{{\"quantity_raw\":{},\"today_quantity_raw\":{},\"average_price_raw\":{},\"mark_price_raw\":{},\"unrealized_pnl_raw\":{},\"margin_raw\":{}}}",
                    json_string(&instrument.to_string()),
                    value.quantity_raw,
                    value.today_quantity_raw,
                    value.average_price_raw,
                    value.mark_price_raw,
                    Self::money_json(value.unrealized_pnl_raw),
                    Self::money_json(value.margin_raw)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let cash = self
            .cash_raw
            .iter()
            .map(|(currency, amount)| format!("{}:{}", json_string(currency), amount))
            .collect::<Vec<_>>()
            .join(",");
        let orders = self
            .orders
            .iter()
            .map(|(id, value)| {
                format!(
                    "{}:{{\"order_id\":{},\"client_order_id\":{},\"instrument\":{},\"side\":{},\"quantity_raw\":{},\"filled_raw\":{},\"status\":{}}}",
                    id,
                    value.order_id,
                    value.client_order_id,
                    json_string(&value.instrument.to_string()),
                    side_code(value.side),
                    value.quantity_raw,
                    value.filled_raw,
                    order_status_code(value.status)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let fills = self
            .fills
            .iter()
            .map(|(id, value)| {
                format!(
                    "{}:{{\"fill_id\":{},\"order_id\":{},\"quantity_raw\":{},\"price_raw\":{},\"fee_raw\":{},\"ts\":{}}}",
                    id,
                    value.fill_id,
                    value.order_id,
                    value.quantity_raw,
                    value.price_raw,
                    value.fee_raw,
                    value.ts
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let transfers = self
            .transfers
            .iter()
            .map(|(id, value)| {
                format!(
                    "{}:{{\"transfer_id\":{},\"currency\":{},\"amount_raw\":{},\"ts\":{}}}",
                    id,
                    value.transfer_id,
                    json_string(&value.currency),
                    value.amount_raw,
                    value.ts
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        // 七个汇总钱字段与上面的 {} 一一对应：equity / available / margin / frozen /
        // realized_pnl / unrealized_pnl / fees / funding（未算过印 null，不印 0）。
        let scalars = self.scalar_json_values();
        format!(
            "{{\"protocol\":\"QIANXING_ACCOUNT\",\"schema_version\":{},\"header\":{{\"snapshot_id\":{},\"account_id\":{},\"portfolio_id\":{},\"venue_id\":{},\"trading_day\":{},\"as_of\":{},\"event_seq\":{},\"state_hash\":{}}},\"cash_raw\":{{{}}},\"equity_raw\":{},\"available_raw\":{},\"margin_raw\":{},\"frozen_raw\":{},\"realized_pnl_raw\":{},\"unrealized_pnl_raw\":{},\"fees_raw\":{},\"funding_raw\":{},\"positions\":{{{}}},\"orders\":{{{}}},\"fills\":{{{}}},\"transfers\":{{{}}},\"reconcile\":{{\"last_reconcile_ts\":{},\"discrepancy_count\":{},\"recovery_state\":{}}}}}",
            self.header.schema_version,
            self.header.snapshot_id,
            json_string(&self.header.account_id),
            json_string(&self.header.portfolio_id),
            json_string(&self.header.venue_id),
            json_string(&self.header.trading_day),
            self.header.as_of,
            self.header.event_seq,
            self.header.state_hash,
            cash,
            scalars[0],
            scalars[1],
            scalars[2],
            scalars[3],
            scalars[4],
            scalars[5],
            scalars[6],
            scalars[7],
            positions,
            orders,
            fills,
            transfers,
            self.reconcile.last_reconcile_ts,
            self.reconcile.discrepancy_count,
            json_string(&self.reconcile.recovery_state),
        )
    }

    /// 解析稳定账户 JSON，并校验协议头、schema 版本、字段一致性和 state hash。
    pub fn from_json(input: &str) -> Result<Self, ProtocolError> {
        let mut value: serde_json::Value = serde_json::from_str(input)
            .map_err(|error| ProtocolError::Serialization(error.to_string()))?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| ProtocolError::Serialization("账户快照必须是 JSON object".into()))?;
        if object.get("protocol").and_then(serde_json::Value::as_str) != Some("QIANXING_ACCOUNT") {
            return Err(ProtocolError::Invalid("账户快照 protocol 非法".into()));
        }
        let top_schema = object
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| ProtocolError::Invalid("账户快照 schema_version 缺失".into()))?;
        let header = object
            .get_mut("header")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| ProtocolError::Invalid("账户快照 header 缺失".into()))?;
        match header
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
        {
            Some(header_schema) if header_schema != top_schema => {
                return Err(ProtocolError::Invalid(
                    "账户快照 schema_version 前后不一致".into(),
                ));
            }
            Some(_) => {}
            None => {
                header.insert("schema_version".into(), serde_json::Value::from(top_schema));
            }
        }
        if let Some(positions) = object
            .get_mut("positions")
            .and_then(serde_json::Value::as_object_mut)
        {
            for (key, value) in positions {
                let instrument = InstrumentId::parse(key).ok_or_else(|| {
                    ProtocolError::Invalid(format!("非法 position instrument: {key}"))
                })?;
                let position = value.as_object_mut().ok_or_else(|| {
                    ProtocolError::Serialization("position 必须是 JSON object".into())
                })?;
                position.entry("instrument").or_insert(
                    serde_json::to_value(instrument)
                        .map_err(|error| ProtocolError::Serialization(error.to_string()))?,
                );
            }
        }
        object.remove("protocol");
        object.remove("schema_version");
        let snapshot: Self = serde_json::from_value(value)
            .map_err(|error| ProtocolError::Serialization(error.to_string()))?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// 供 Rust/Python/其他 serde 实现使用的无损 JSON 往返格式。
    pub fn to_wire_json(&self) -> Result<String, ProtocolError> {
        self.validate()?;
        serde_json::to_string(self).map_err(|error| ProtocolError::Serialization(error.to_string()))
    }

    pub fn from_wire_json(input: &str) -> Result<Self, ProtocolError> {
        let snapshot: Self = serde_json::from_str(input)
            .map_err(|error| ProtocolError::Serialization(error.to_string()))?;
        snapshot.validate()?;
        Ok(snapshot)
    }
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct ScalarState {
    equity_raw: Option<i128>,
    available_raw: Option<i128>,
    margin_raw: Option<i128>,
    frozen_raw: Option<i128>,
    realized_pnl_raw: Option<i128>,
    unrealized_pnl_raw: Option<i128>,
    fees_raw: Option<i128>,
    funding_raw: Option<i128>,
    reconcile: ReconcileSnapshot,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Change<K, V> {
    Upsert { key: K, value: V },
    Remove { key: K },
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SnapshotDiff {
    pub schema_version: u32,
    pub base_state_hash: u64,
    pub target_state_hash: u64,
    pub target_header: SnapshotHeader,
    pub cash: Vec<Change<String, i128>>,
    pub positions: Vec<Change<InstrumentId, PositionSnapshot>>,
    pub orders: Vec<Change<u64, OrderSnapshot>>,
    pub fills: Vec<Change<u64, FillSnapshot>>,
    pub transfers: Vec<Change<u64, TransferSnapshot>>,
    replacement: Option<ScalarState>,
}

impl SnapshotDiff {
    pub fn apply(&self, base: &AccountSnapshot) -> Result<AccountSnapshot, ProtocolError> {
        if base.state_hash() != self.base_state_hash {
            return Err(ProtocolError::BaseStateMismatch);
        }
        let mut next = base.clone();
        if next.header.account_id != self.target_header.account_id
            || next.header.venue_id != self.target_header.venue_id
            || self.target_header.schema_version != self.schema_version
        {
            return Err(ProtocolError::IdentityMismatch);
        }
        next.header = self.target_header.clone();
        next.header.state_hash = 0;
        apply_map(&mut next.cash_raw, &self.cash);
        apply_map(&mut next.positions, &self.positions);
        apply_map(&mut next.orders, &self.orders);
        apply_map(&mut next.fills, &self.fills);
        apply_map(&mut next.transfers, &self.transfers);
        if let Some(s) = &self.replacement {
            next.equity_raw = s.equity_raw;
            next.available_raw = s.available_raw;
            next.margin_raw = s.margin_raw;
            next.frozen_raw = s.frozen_raw;
            next.realized_pnl_raw = s.realized_pnl_raw;
            next.unrealized_pnl_raw = s.unrealized_pnl_raw;
            next.fees_raw = s.fees_raw;
            next.funding_raw = s.funding_raw;
            next.reconcile = s.reconcile.clone();
        }
        next.header.state_hash = 0;
        if next.state_hash() != self.target_state_hash {
            return Err(ProtocolError::TargetStateMismatch);
        }
        next.header.state_hash = self.target_state_hash;
        Ok(next)
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct QifiEnvelope {
    pub protocol: String,
    pub version: String,
    pub snapshot: AccountSnapshot,
}

impl QifiEnvelope {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("QIFI envelope is serializable")
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.protocol.trim() != "QIFI" || self.version.trim().is_empty() {
            return Err(ProtocolError::Invalid("QIFI envelope 头非法".into()));
        }
        self.snapshot.validate()
    }

    pub fn from_json(input: &str) -> Result<Self, ProtocolError> {
        let envelope: Self = serde_json::from_str(input)
            .map_err(|error| ProtocolError::Serialization(error.to_string()))?;
        envelope.validate()?;
        Ok(envelope)
    }
}

/// 快照持久化边界。实现可以替换为对象存储/数据库，但必须先校验和封存快照。
pub trait SnapshotStore {
    fn save(&self, snapshot: &AccountSnapshot) -> Result<PathBuf, ProtocolError>;
    fn load_json(&self, snapshot_id: u64, state_hash: u64) -> Result<String, ProtocolError>;
}

pub struct FileSnapshotStore {
    root: PathBuf,
}

impl FileSnapshotStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl SnapshotStore for FileSnapshotStore {
    fn save(&self, snapshot: &AccountSnapshot) -> Result<PathBuf, ProtocolError> {
        snapshot.validate()?;
        std::fs::create_dir_all(&self.root)
            .map_err(|error| ProtocolError::Io(error.to_string()))?;
        let content = snapshot.to_json();
        let filename = format!(
            "account-{}-{:016x}.json",
            snapshot.header.snapshot_id,
            snapshot.state_hash()
        );
        let target = self.root.join(filename);
        if target.exists() {
            let existing = std::fs::read_to_string(&target)
                .map_err(|error| ProtocolError::Io(error.to_string()))?;
            if existing == content {
                return Ok(target);
            }
            return Err(ProtocolError::StateHashMismatch);
        }
        let temp = self.root.join(format!(
            ".account-{}-{:016x}.json.tmp.{}",
            snapshot.header.snapshot_id,
            snapshot.state_hash(),
            SNAPSHOT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&temp, content).map_err(|error| ProtocolError::Io(error.to_string()))?;
        sync_snapshot_file(&temp)?;
        std::fs::rename(&temp, &target).map_err(|error| ProtocolError::Io(error.to_string()))?;
        Ok(target)
    }

    fn load_json(&self, snapshot_id: u64, state_hash: u64) -> Result<String, ProtocolError> {
        let path = self
            .root
            .join(format!("account-{}-{:016x}.json", snapshot_id, state_hash));
        let content =
            std::fs::read_to_string(path).map_err(|error| ProtocolError::Io(error.to_string()))?;
        let snapshot = AccountSnapshot::from_json(&content)?;
        if snapshot.header.snapshot_id != snapshot_id || snapshot.state_hash() != state_hash {
            return Err(ProtocolError::StateHashMismatch);
        }
        Ok(content)
    }
}

fn sync_snapshot_file(path: &Path) -> Result<(), ProtocolError> {
    #[cfg(not(windows))]
    {
        std::fs::File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| ProtocolError::Io(error.to_string()))?;
    }
    #[cfg(windows)]
    {
        // Windows file hooks can reject fsync on a fresh temp file; rename is
        // still the visibility boundary used by this single-host store.
        let _ = path;
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ProtocolError {
    Invalid(String),
    IdentityMismatch,
    StateHashMismatch,
    BaseStateMismatch,
    TargetStateMismatch,
    Io(String),
    Serialization(String),
}

fn diff_map<K: Ord + Clone, V: Clone + PartialEq>(
    base: &BTreeMap<K, V>,
    target: &BTreeMap<K, V>,
) -> Vec<Change<K, V>> {
    let mut out = Vec::new();
    for (key, value) in target {
        if base.get(key) != Some(value) {
            out.push(Change::Upsert {
                key: key.clone(),
                value: value.clone(),
            });
        }
    }
    for key in base.keys() {
        if !target.contains_key(key) {
            out.push(Change::Remove { key: key.clone() });
        }
    }
    out
}

fn apply_map<K: Ord + Clone, V: Clone>(map: &mut BTreeMap<K, V>, changes: &[Change<K, V>]) {
    for change in changes {
        match change {
            Change::Upsert { key, value } => {
                map.insert(key.clone(), value.clone());
            }
            Change::Remove { key } => {
                map.remove(key);
            }
        }
    }
}

fn side_code(side: Side) -> u64 {
    match side {
        Side::Buy => 1,
        Side::Sell => 2,
    }
}

fn order_status_code(status: OrderStatus) -> u64 {
    status as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> AccountSnapshot {
        let instrument = InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
        let mut snapshot = AccountSnapshot::new(1, "main", "default", "BINANCE", 10);
        snapshot.cash_raw.insert("USDT".into(), 1000);
        snapshot.positions.insert(
            instrument.clone(),
            PositionSnapshot {
                instrument,
                quantity_raw: 2,
                ..PositionSnapshot::default()
            },
        );
        snapshot.seal();
        snapshot
    }

    #[test]
    fn diff_is_deterministic_and_replayable() {
        let base = snapshot();
        let mut target = base.clone();
        target.cash_raw.insert("USDT".into(), 900);
        target.equity_raw = Some(900);
        target.header.snapshot_id = 2;
        target.header.as_of = 11;
        target.header.event_seq = 4;
        target.seal();
        let diff = base.diff(&target).unwrap();
        let rebuilt = diff.apply(&base).unwrap();
        assert_eq!(rebuilt, target);
    }

    #[test]
    fn wrong_base_is_rejected() {
        let base = snapshot();
        let mut target = base.clone();
        target.cash_raw.insert("USDT".into(), 900);
        target.seal();
        let diff = base.diff(&target).unwrap();
        let mut wrong = base.clone();
        wrong.cash_raw.insert("USDT".into(), 800);
        wrong.seal();
        assert_eq!(diff.apply(&wrong), Err(ProtocolError::BaseStateMismatch));
    }

    #[test]
    fn json_wire_format_is_stable_and_uses_raw_integers() {
        let snapshot = snapshot();
        let json = snapshot.to_json();
        assert!(json.starts_with("{\"protocol\":\"QIANXING_ACCOUNT\""));
        assert!(json.contains("\"cash_raw\":{\"USDT\":1000}"));
        assert!(json.contains("\"quantity_raw\":2"));
        assert!(ACCOUNT_SNAPSHOT_JSON_SCHEMA.contains("QIANXING_ACCOUNT"));
        let wire = snapshot.to_wire_json().unwrap();
        assert_eq!(AccountSnapshot::from_wire_json(&wire).unwrap(), snapshot);
        let qifi = snapshot.to_qifi();
        let qifi_json = qifi.to_json();
        assert!(qifi_json.contains("\"protocol\":\"QIFI\""));
        assert_eq!(QifiEnvelope::from_json(&qifi_json).unwrap(), qifi);
    }

    #[test]
    fn stable_json_and_file_snapshot_store_are_recoverable_and_idempotent() {
        let snapshot = snapshot();
        let root = std::env::temp_dir().join(format!(
            "qianxing-protocol-{}-{}",
            std::process::id(),
            snapshot.header.snapshot_id
        ));
        let store = FileSnapshotStore::new(&root);
        let first = store.save(&snapshot).unwrap();
        let second = store.save(&snapshot).unwrap();
        assert_eq!(first, second);
        let json = store
            .load_json(snapshot.header.snapshot_id, snapshot.state_hash())
            .unwrap();
        assert_eq!(AccountSnapshot::from_json(&json).unwrap(), snapshot);
    }
}
