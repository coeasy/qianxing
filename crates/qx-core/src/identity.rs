//! 身份模型：多交易所的地基。
//!
//! 三层分离、绝不相互覆盖：
//! - [`CanonicalProduct`] 研究表达（如 `BTC-USDT`）
//! - [`InstrumentId`]   执行与估值（如 `BTCUSDT-PERP.BINANCE`）
//! - 数据血缘由 `qx_guanxing` 的 `DataSourceId` 承担
//!
//! 子账户必须并入 [`VenueId`]，否则同一 API key 下不同保证金模式/权限会被错误合并。

use serde::{Deserialize, Serialize};
use std::fmt;

/// 交易场所标识 = provider + region + account_set。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct VenueId(pub String);

impl VenueId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into().to_uppercase())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for VenueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl fmt::Display for VenueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 可交易工具标识 = native symbol + venue，点号分隔。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InstrumentId {
    pub symbol: String,
    pub venue: VenueId,
}

impl InstrumentId {
    pub fn new(symbol: impl Into<String>, venue: VenueId) -> Self {
        Self {
            symbol: symbol.into(),
            venue,
        }
    }

    /// 解析 `"BTCUSDT-PERP.BINANCE"` → symbol=`BTCUSDT-PERP`, venue=`BINANCE`
    pub fn parse(s: &str) -> Option<Self> {
        let (sym, ven) = s.rsplit_once('.')?;
        if sym.is_empty() || ven.is_empty() {
            return None;
        }
        Some(Self {
            symbol: sym.to_string(),
            venue: VenueId::new(ven),
        })
    }
}

impl fmt::Debug for InstrumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.symbol, self.venue)
    }
}
impl fmt::Display for InstrumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.symbol, self.venue)
    }
}

/// 交易所原生市场标识 = venue + raw_symbol（用于符号映射）。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct MarketId {
    pub venue: VenueId,
    pub raw_symbol: String,
}

impl fmt::Display for MarketId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.venue, self.raw_symbol)
    }
}

/// 规范化产品：只用于研究与策略表达，不用于成交。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct CanonicalProduct {
    pub base: String,
    pub quote: String,
}

impl CanonicalProduct {
    pub fn new(base: impl Into<String>, quote: impl Into<String>) -> Self {
        Self {
            base: base.into().to_uppercase(),
            quote: quote.into().to_uppercase(),
        }
    }
}

impl fmt::Display for CanonicalProduct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.base, self.quote)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_instrument_id() {
        let id = InstrumentId::parse("BTCUSDT-PERP.BINANCE").unwrap();
        assert_eq!(id.symbol, "BTCUSDT-PERP");
        assert_eq!(id.venue.as_str(), "BINANCE");
        assert_eq!(format!("{}", id), "BTCUSDT-PERP.BINANCE");
    }

    #[test]
    fn canonical_is_not_instrument() {
        let c = CanonicalProduct::new("btc", "usdt");
        assert_eq!(format!("{}", c), "BTC-USDT");
    }
}
