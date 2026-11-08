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

/// 没人声明时，账户账簿用哪种币记账 —— 全仓唯一写点。
///
/// 三条回落链都从这里取值：worker 没写 `settlement_currency`、同一账户日志的写入方
/// 全都缺席、回测侧 market spec 没结算币种。字面量分头抄写时，一处改大盘子就跟着分叉
/// （V13 §4 L2-6），所以 `tools/check_architecture.py` 的 `settlement_currency_check()`
/// 钉住"生产代码里不得再出现 `"USDT"` 整串字面量"。
///
/// 它只是**缺省值**，不是合法币种白名单：任何非空字符串都是合法的 `settlement_currency`。
pub const DEFAULT_SETTLEMENT_CURRENCY: &str = "USDT";

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

    /// 标的/产品级的 Binance 判定，全仓唯一一处：读的是 `InstrumentId` 里已经解析过的
    /// venue 段（`BTCUSDT-PERP.BINANCE`），它必须是精确的一家交易所。
    ///
    /// 口径与 [`crate::VenueFamily::parse`] **故意不同**：`binance-testnet` 是 worker 的
    /// 账户域（那里按子串认，见该模块说明），不是一种产品 venue，所以这里不能改成子串。
    pub fn is_binance(&self) -> bool {
        self.0.eq_ignore_ascii_case("BINANCE")
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
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct InstrumentId {
    symbol: String,
    pub venue: VenueId,
}

impl InstrumentId {
    /// Explicit sentinel identity for absent/legacy wire fields; it is not a tradeable product.
    pub fn unknown() -> Self {
        Self {
            symbol: "UNKNOWN".into(),
            venue: VenueId::new("UNKNOWN"),
        }
    }

    /// Shared lexical guard for symbols received through textual identity boundaries.
    /// Supports common spot (`BTC-USDT`, `BTC/USDT`) and derivative
    /// (`BTC/USDT:USDT`) forms while excluding path and request syntax.
    pub fn is_valid_symbol(symbol: &str) -> bool {
        if symbol.is_empty()
            || symbol.len() > 128
            || symbol.starts_with(|c: char| {
                c.is_ascii_alphabetic() && symbol.as_bytes().get(1) == Some(&b':')
            })
        {
            return false;
        }
        let mut previous_separator = true;
        for byte in symbol.bytes() {
            if byte.is_ascii_alphanumeric() {
                previous_separator = false;
            } else if matches!(byte, b'/' | b':' | b'_' | b'-') && !previous_separator {
                previous_separator = true;
            } else {
                return false;
            }
        }
        !previous_separator
    }

    /// Construct a typed identity after validating the native symbol.
    pub fn new(symbol: impl Into<String>, venue: VenueId) -> Option<Self> {
        let symbol = symbol.into();
        Self::is_valid_symbol(&symbol).then_some(Self { symbol, venue })
    }

    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// 解析 `"BTCUSDT-PERP.BINANCE"` → symbol=`BTCUSDT-PERP`, venue=`BINANCE`
    pub fn parse(s: &str) -> Option<Self> {
        let (sym, ven) = s.rsplit_once('.')?;
        if !Self::is_valid_symbol(sym) || ven.is_empty() {
            return None;
        }
        Self::new(sym, VenueId::new(ven))
    }
}

impl<'de> Deserialize<'de> for InstrumentId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawInstrumentId {
            symbol: String,
            venue: VenueId,
        }

        let raw = RawInstrumentId::deserialize(deserializer)?;
        if !InstrumentId::is_valid_symbol(&raw.symbol) {
            return Err(serde::de::Error::custom("invalid instrument symbol"));
        }
        Ok(Self {
            symbol: raw.symbol,
            venue: raw.venue,
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
        assert_eq!(id.symbol(), "BTCUSDT-PERP");
        assert_eq!(id.venue.as_str(), "BINANCE");
        assert_eq!(format!("{}", id), "BTCUSDT-PERP.BINANCE");
    }

    #[test]
    fn canonical_is_not_instrument() {
        let c = CanonicalProduct::new("btc", "usdt");
        assert_eq!(format!("{}", c), "BTC-USDT");
    }

    #[test]
    fn instrument_symbol_validation_accepts_market_forms_and_rejects_path_syntax() {
        for symbol in ["BTCUSDT", "000001", "BTC-USDT", "BTC/USDT:USDT", "BTC_USDT"] {
            assert!(InstrumentId::is_valid_symbol(symbol), "{symbol}");
            assert!(InstrumentId::parse(&format!("{symbol}.BINANCE")).is_some());
        }
        for symbol in [
            "",
            "../../etc/passwd",
            "/etc/passwd",
            "C:temp",
            "BTC?USDT",
            "BTC\nUSDT",
            "BTC.USDT",
            "BTC--USDT",
            "BTC/",
            "é",
        ] {
            assert!(!InstrumentId::is_valid_symbol(symbol), "{symbol:?}");
            assert!(InstrumentId::parse(&format!("{symbol}.BINANCE")).is_none());
        }
        assert!(!InstrumentId::is_valid_symbol(&"A".repeat(129)));
        assert!(InstrumentId::new("BTC/USDT:USDT", VenueId::new("BINANCE")).is_some());
        assert!(InstrumentId::new("../../etc/passwd", VenueId::new("BINANCE")).is_none());
    }

    #[test]
    fn instrument_id_deserialization_uses_the_shared_symbol_guard() {
        let id = InstrumentId::new("BTC/USDT:USDT", VenueId::new("BINANCE")).unwrap();
        let serialized = serde_json::to_value(&id).unwrap();
        assert_eq!(serialized["symbol"], "BTC/USDT:USDT");
        assert_eq!(serialized["venue"], "BINANCE");
        assert!(serde_json::from_value::<InstrumentId>(serialized).is_ok());
        assert!(serde_json::from_str::<InstrumentId>(
            r#"{"symbol":"../../etc/passwd","venue":"BINANCE"}"#
        )
        .is_err());
    }
}
