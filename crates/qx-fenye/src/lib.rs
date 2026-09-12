//! # qx-fenye — 分野
//!
//! 身份与市场：合约规格、市场状态、双向版本化符号映射。
//!
//! 两条铁律：
//! 1. `InstrumentSpec`（不可变、版本化）与 `MarketState`（可刷新）**必须分离**。
//! 2. 符号映射**只能新增与退役，不能覆盖或静默删除**——否则历史特征、回测路径
//!    与真实成交都将失去可解释性。

use qx_core::QxError;
use std::collections::BTreeMap;

/// 生命周期：元数据未完整时禁止下单。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lifecycle {
    Unknown,
    Loading,
    Ready,
    Halted,
    Retired,
}

impl Lifecycle {
    /// 是否允许新开仓。
    pub fn can_open(self) -> bool {
        matches!(self, Lifecycle::Ready)
    }

    /// 是否允许平仓（Halted 仍允许减仓）。
    pub fn can_close(self) -> bool {
        matches!(self, Lifecycle::Ready | Lifecycle::Halted)
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Lifecycle::Retired)
    }
}

/// 合约规格：不可变、版本化。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct InstrumentSpec {
    pub symbol: String,
    pub venue_id: String,
    pub instrument_type: String,
    pub quote_currency: String,
    pub settlement_currency: String,
    /// 合约乘数。
    pub multiplier: u32,
    /// 最小价格变动。
    pub price_tick: i128,
    /// 最小数量变动。
    pub qty_step: i128,
    pub min_qty: i128,
    /// 规格版本，刷新时递增。
    pub version: u32,
    /// maker 费率（基点，万分之一）。
    pub maker_bp: i64,
    /// taker 费率（基点）。
    pub taker_bp: i64,
    pub valid_from: u64,
    pub valid_to: Option<u64>,
}

impl InstrumentSpec {
    pub fn validate(&self) -> Result<(), QxError> {
        if self.symbol.trim().is_empty()
            || self.venue_id.trim().is_empty()
            || self.instrument_type.trim().is_empty()
            || self.quote_currency.trim().is_empty()
            || self.settlement_currency.trim().is_empty()
            || self.multiplier == 0
            || self.price_tick <= 0
            || self.qty_step <= 0
            || self.min_qty <= 0
            || self.valid_to.is_some_and(|to| to <= self.valid_from)
        {
            return Err(QxError::BusinessViolation("InstrumentSpec 字段非法".into()));
        }
        Ok(())
    }

    /// 把数量对齐到 qty_step。
    pub fn align_qty(&self, qty: i128) -> i128 {
        if self.qty_step <= 0 {
            return qty;
        }
        (qty / self.qty_step) * self.qty_step
    }

    pub fn notional(&self, qty: i128, price: i128) -> Option<i128> {
        qty.checked_mul(price)?.checked_mul(self.multiplier as i128)
    }

    pub fn validate_order(&self, qty: i128, price: Option<i128>) -> Result<(), QxError> {
        if qty < self.min_qty {
            return Err(QxError::BusinessViolation("订单数量低于最小数量".into()));
        }
        if self.qty_step > 0 && qty % self.qty_step != 0 {
            return Err(QxError::BusinessViolation("订单数量不满足 lot step".into()));
        }
        if let Some(px) = price {
            if px <= 0 {
                return Err(QxError::BusinessViolation("价格必须为正".into()));
            }
            if self.price_tick > 0 && px % self.price_tick != 0 {
                return Err(QxError::BusinessViolation("订单价格不满足 tick".into()));
            }
        }
        Ok(())
    }
}

/// 市场状态：可刷新、与规格分离。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MarketState {
    pub lifecycle: Lifecycle,
    pub last_price: Option<i128>,
    pub updated_ts: u64,
}

impl Default for MarketState {
    fn default() -> Self {
        Self {
            lifecycle: Lifecycle::Unknown,
            last_price: None,
            updated_ts: 0,
        }
    }
}

impl MarketState {
    /// 下单前置校验：元数据未完整 → 拒绝，而不是"尽力而为"。
    pub fn check_tradable(&self) -> Result<(), QxError> {
        match self.lifecycle {
            Lifecycle::Ready => Ok(()),
            Lifecycle::Loading | Lifecycle::Unknown => {
                Err(QxError::VenueState("元数据未就绪，禁止下单".into()))
            }
            Lifecycle::Halted => Err(QxError::VenueState("已停牌，禁止新开仓".into())),
            Lifecycle::Retired => Err(QxError::Permanent("合约已退市".into())),
        }
    }
}

/// 映射键：venue + 原生 symbol + 版本。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct MappingKey {
    pub venue: String,
    pub raw_symbol: String,
    pub version: u32,
}

/// 映射记录：除字符串外还保留生效区间与原始定义哈希。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Mapping {
    pub standard: String,
    pub effective_from: u64,
    /// None 表示仍然生效。
    pub effective_to: Option<u64>,
    /// 原始 market JSON 的哈希，用于检测交易所侧变更。
    pub raw_hash: u64,
}

/// 不可变合约规格注册表：版本只能新增，历史规格只能结束有效期，不能覆盖。
#[derive(Default)]
pub struct InstrumentRegistry {
    specs: BTreeMap<(String, String, u32), InstrumentSpec>,
}

impl InstrumentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, spec: InstrumentSpec) -> Result<(), QxError> {
        spec.validate()?;
        let key = (spec.venue_id.clone(), spec.symbol.clone(), spec.version);
        if self.specs.contains_key(&key) {
            return Err(QxError::Invariant(
                "InstrumentSpec 版本已存在，拒绝覆盖".into(),
            ));
        }
        for ((venue, symbol, _), existing) in &self.specs {
            if venue == &spec.venue_id
                && symbol == &spec.symbol
                && existing.valid_from < spec.valid_to.unwrap_or(u64::MAX)
                && spec.valid_from < existing.valid_to.unwrap_or(u64::MAX)
            {
                return Err(QxError::Invariant(
                    "同一合约规格有效期重叠，必须先结束旧版本".into(),
                ));
            }
        }
        self.specs.insert(key, spec);
        Ok(())
    }

    pub fn as_of(&self, venue: &str, symbol: &str, event_time: u64) -> Option<&InstrumentSpec> {
        self.specs
            .iter()
            .filter(|((v, s, _), spec)| {
                v == venue
                    && s == symbol
                    && spec.valid_from <= event_time
                    && spec.valid_to.is_none_or(|to| event_time < to)
            })
            .max_by_key(|((_, _, version), _)| *version)
            .map(|(_, spec)| spec)
    }

    pub fn len(&self) -> usize {
        self.specs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }
}

/// 双向符号映射表。
///
/// 用 `BTreeMap` 而非 `HashMap`：**遍历顺序必须确定**，否则装配与快照不可复现。
#[derive(Default)]
pub struct SymbolMapper {
    by_raw: std::collections::BTreeMap<MappingKey, Mapping>,
}

impl SymbolMapper {
    pub fn new() -> Self {
        Self::default()
    }

    /// 新增映射。**已存在则拒绝覆盖**——这是防止历史失忆的关键。
    pub fn insert(&mut self, key: MappingKey, m: Mapping) -> Result<(), QxError> {
        if m.effective_to.is_some_and(|to| to <= m.effective_from) {
            return Err(QxError::BusinessViolation(
                "映射生效区间必须满足 effective_from < effective_to".into(),
            ));
        }
        if self.by_raw.contains_key(&key) {
            return Err(QxError::Invariant(format!(
                "映射已存在，拒绝覆盖: {:?}",
                key
            )));
        }
        let overlaps = |left: &Mapping, right: &Mapping| {
            let left_end = left.effective_to.unwrap_or(u64::MAX);
            let right_end = right.effective_to.unwrap_or(u64::MAX);
            left.effective_from < right_end && right.effective_from < left_end
        };
        if self.by_raw.iter().any(|(existing_key, existing)| {
            existing_key.venue == key.venue
                && existing_key.raw_symbol == key.raw_symbol
                && overlaps(existing, &m)
        }) {
            return Err(QxError::Invariant(
                "同一原生 symbol 的映射生效区间重叠，必须先退役旧版本".into(),
            ));
        }
        self.by_raw.insert(key, m);
        Ok(())
    }

    /// 退役：标记 effective_to，**不删除**。
    pub fn retire(&mut self, key: &MappingKey, at: u64) -> Result<(), QxError> {
        match self.by_raw.get_mut(key) {
            Some(m) => {
                if m.effective_to.is_some() {
                    return Err(QxError::Invariant("映射已退役，拒绝重复退役".into()));
                }
                if at <= m.effective_from {
                    return Err(QxError::BusinessViolation(
                        "映射退役时间必须晚于生效时间".into(),
                    ));
                }
                m.effective_to = Some(at);
                Ok(())
            }
            None => Err(QxError::Permanent("映射不存在".into())),
        }
    }

    pub fn get(&self, key: &MappingKey) -> Option<&Mapping> {
        self.by_raw.get(key)
    }

    /// 查最新有效版本的标准化 symbol。
    pub fn standard_of(&self, venue: &str, raw_symbol: &str) -> Option<String> {
        self.standard_of_as_of(venue, raw_symbol, u64::MAX)
    }

    /// 按事件时间选择当时可见的映射，禁止把未来版本带入历史回测。
    pub fn standard_of_as_of(
        &self,
        venue: &str,
        raw_symbol: &str,
        event_time: u64,
    ) -> Option<String> {
        self.by_raw
            .iter()
            .filter(|(k, m)| {
                k.venue == venue
                    && k.raw_symbol == raw_symbol
                    && m.effective_from <= event_time
                    && m.effective_to.is_none_or(|to| event_time < to)
            })
            .max_by_key(|(k, _)| k.version)
            .map(|(_, m)| m.standard.clone())
    }

    pub fn len(&self) -> usize {
        self.by_raw.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_raw.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(v: u32) -> MappingKey {
        MappingKey {
            venue: "BINANCE".into(),
            raw_symbol: "BTCUSDT".into(),
            version: v,
        }
    }

    #[test]
    fn cannot_overwrite_existing_mapping() {
        let mut m = SymbolMapper::new();
        let mk = Mapping {
            standard: "BTC-USDT".into(),
            effective_from: 0,
            effective_to: None,
            raw_hash: 1,
        };
        assert!(m.insert(key(1), mk.clone()).is_ok());
        assert!(m.insert(key(1), mk).is_err());
    }

    #[test]
    fn retire_keeps_history() {
        let mut m = SymbolMapper::new();
        m.insert(
            key(1),
            Mapping {
                standard: "BTC-USDT".into(),
                effective_from: 0,
                effective_to: None,
                raw_hash: 1,
            },
        )
        .unwrap();
        m.retire(&key(1), 100).unwrap();
        assert_eq!(m.get(&key(1)).unwrap().effective_to, Some(100));
        assert_eq!(m.len(), 1); // 未删除
    }

    #[test]
    fn mapping_is_point_in_time() {
        let mut m = SymbolMapper::new();
        m.insert(
            key(1),
            Mapping {
                standard: "BTC-USDT-OLD".into(),
                effective_from: 0,
                effective_to: Some(100),
                raw_hash: 1,
            },
        )
        .unwrap();
        m.insert(
            key(2),
            Mapping {
                standard: "BTC-USDT".into(),
                effective_from: 100,
                effective_to: None,
                raw_hash: 2,
            },
        )
        .unwrap();
        assert_eq!(
            m.standard_of_as_of("BINANCE", "BTCUSDT", 99).as_deref(),
            Some("BTC-USDT-OLD")
        );
        assert_eq!(
            m.standard_of_as_of("BINANCE", "BTCUSDT", 100).as_deref(),
            Some("BTC-USDT")
        );
    }

    #[test]
    fn halted_rejects_open_but_allows_close() {
        let s = MarketState {
            lifecycle: Lifecycle::Halted,
            last_price: None,
            updated_ts: 0,
        };
        assert!(!s.lifecycle.can_open());
        assert!(s.lifecycle.can_close());
        assert!(s.check_tradable().is_err());
    }

    #[test]
    fn instrument_spec_enforces_lot_and_tick() {
        let s = InstrumentSpec {
            symbol: "T".into(),
            venue_id: "SIM".into(),
            instrument_type: "SPOT".into(),
            quote_currency: "USD".into(),
            settlement_currency: "USD".into(),
            multiplier: 1,
            price_tick: 1_000_000_000,
            qty_step: 1_000_000_000,
            min_qty: 1_000_000_000,
            version: 1,
            maker_bp: 0,
            taker_bp: 0,
            valid_from: 0,
            valid_to: None,
        };
        assert!(s.validate().is_ok());
        assert!(s
            .validate_order(1_000_000_000, Some(100_000_000_000))
            .is_ok());
        assert!(s
            .validate_order(500_000_000, Some(100_000_000_000))
            .is_err());
    }

    #[test]
    fn instrument_registry_is_versioned_and_point_in_time() {
        let mut registry = InstrumentRegistry::new();
        let base = InstrumentSpec {
            symbol: "T".into(),
            venue_id: "SIM".into(),
            instrument_type: "SPOT".into(),
            quote_currency: "USD".into(),
            settlement_currency: "USD".into(),
            multiplier: 1,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            version: 1,
            maker_bp: 1,
            taker_bp: 2,
            valid_from: 0,
            valid_to: Some(100),
        };
        registry.register(base.clone()).unwrap();
        assert_eq!(registry.as_of("SIM", "T", 99).unwrap().version, 1);
        let mut next = base.clone();
        next.version = 2;
        next.valid_from = 100;
        next.valid_to = None;
        registry.register(next).unwrap();
        assert_eq!(registry.as_of("SIM", "T", 100).unwrap().version, 2);
        assert!(registry.register(base).is_err());
    }
}
