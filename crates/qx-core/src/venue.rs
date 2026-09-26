//! `venue_id` 自由文本到 Venue 家族的唯一映射。
//!
//! 运行拓扑里有六处要问"这个 worker 属于哪个 Venue 家族"：编排器决定给它起哪个
//! `*-worker` 进程、`*-worker` 入口自检自己的绑定、`live-check` 判定 production 里
//! 不许出现 Paper 腿、账户 EventLog 命名取前缀、Paper 名义资金字段策略、可选标的
//! 规格校验的 Paper 豁免。收拢到这里之前，同一个问题在仓库里有 19 处写法
//! （9 处问"是不是 Binance"、10 处问"是不是 Paper"：`.map(..).contains("binance")
//! == Some(true)`、`is_some_and(..)`、`map(..).unwrap_or(..)`、局部 `fn is_binance`），
//! 其中任何一处改口径都不会带着其余十几处一起改。
//!
//! Binance 一侧必须是**大小写无关的子串**而不是整名相等：仓内 `venue_id` 实测有
//! `binance`、`BINANCE`、`binance-testnet` 三种取值，整名相等会把 testnet 那批
//! 静默判成"不是 Binance"。Paper 一侧反过来必须整名相等：`paper-proxy` 不是 Paper 域。

/// Venue 家族。只登记"内核有专用私有接口"的两家，其余统一是 `Other`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VenueFamily {
    /// 本地 Paper 撮合域：整名相等（大小写与首尾空白无关）。
    Paper,
    /// Binance 家族：子串匹配，`binance` / `BINANCE` / `binance-testnet` 都算。
    Binance,
    /// 配了 `venue_id` 但不属于上面两家，走 CCXT 通用路径（如 `okx`）。
    Other,
}

impl VenueFamily {
    /// 一次解析：全仓唯一一处"自由文本 → 家族"的判定，第二处由门禁拒绝。
    pub fn parse(venue_id: &str) -> Self {
        let normalized = venue_id.trim().to_ascii_lowercase();
        if normalized == "paper" {
            Self::Paper
        } else if normalized.contains("binance") {
            Self::Binance
        } else {
            Self::Other
        }
    }

    /// `venue_id` 缺席时仍是 `None`：按内核底线第 9 条，"没配 Venue"不是 `Other`
    /// （`Other` 是一个真实的 CCXT venue），把缺席折进合法值等于替配置报值。
    pub fn parse_option(venue_id: Option<&str>) -> Option<Self> {
        venue_id.map(Self::parse)
    }
}

#[cfg(test)]
mod tests {
    use super::VenueFamily;

    #[test]
    fn binance_family_is_matched_by_substring_regardless_of_case() {
        for venue in [
            "binance",
            "BINANCE",
            "Binance",
            "binance-testnet",
            " binance ",
        ] {
            assert_eq!(
                VenueFamily::parse(venue),
                VenueFamily::Binance,
                "{venue} 必须认成 Binance 家族，否则 testnet 与大小写写法会被静默判成非 Binance"
            );
        }
    }

    #[test]
    fn paper_is_exact_and_a_paper_prefixed_venue_is_not_paper() {
        assert_eq!(VenueFamily::parse("paper"), VenueFamily::Paper);
        assert_eq!(VenueFamily::parse(" Paper "), VenueFamily::Paper);
        assert_eq!(VenueFamily::parse("paper-proxy"), VenueFamily::Other);
        assert_eq!(VenueFamily::parse("okx"), VenueFamily::Other);
        assert_eq!(VenueFamily::parse(""), VenueFamily::Other);
    }

    #[test]
    fn absent_venue_stays_absent_instead_of_becoming_other() {
        assert_eq!(VenueFamily::parse_option(None), None);
        assert_eq!(
            VenueFamily::parse_option(Some("okx")),
            Some(VenueFamily::Other),
            "有值才可能是 Other"
        );
    }
}
