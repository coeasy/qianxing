//! 契约适配器（§7 M1）：把**同名兄弟**之间的字段映射收成单点。
//!
//! `qx_data::Bar`（摄取层：`instrument` + 原始列 + `timestamp`）与 `qx_guanxing::Bar`（市场数据层：
//! 无 `instrument` 的列式 OHLCV + `ts`）在仓内是两个真实存在的类型——按 DD-1 **不合并**（纯内核
//! 不许反向依赖上层 crate）。合并之前，两边的一致性检查是**手抄的字段映射**：五行比较散在调用点里，
//! 谁改了字段名都只能靠编译器在那一处抓，第二处漏改则静默放过。
//!
//! 这里把映射收成单点。`qx_core::contract::CONTRACT_MATRIX` 的 `market_data_bar` 行点名这一对，
//! `adapter` 字段指向本模块；`tools/check_architecture.py` 的 `contract_matrix_check` 核对
//! 「这个 adapter 真有生产读者」，所以它不是登记完就躺着。

use qx_data::Bar as IngestBar;
use qx_guanxing::Bar as MarketBar;

/// 摄取层的 Bar 与市场数据层的 Bar 是否逐格相等（时间戳 + 五列 OHLCV）。
///
/// 两个类型**不同名同形**：摄取层多带 `instrument`（调用方另有来源），时间戳字段叫 `timestamp`
/// 而不是 `ts`，价格/量列叫 `*_raw` 而不是裸名。这层改名就是"命名转换矩阵"要收的那件事——
/// 调用点只写意图，改名只在这里发生一次。
pub(crate) fn ingest_bar_matches_market_bar(ingest: &IngestBar, market: &MarketBar) -> bool {
    ingest.timestamp == market.ts
        && ingest.open_raw == market.open
        && ingest.high_raw == market.high
        && ingest.low_raw == market.low
        && ingest.close_raw == market.close
        && ingest.volume_raw == market.volume
}

#[cfg(test)]
mod tests {
    use super::*;

    fn market(ts: u64) -> MarketBar {
        MarketBar::new(ts, 10, 11, 9, 10, 3)
    }

    fn ingest(ts: u64) -> IngestBar {
        IngestBar {
            instrument: "000001.SZSE".into(),
            timestamp: ts,
            open_raw: 10,
            high_raw: 11,
            low_raw: 9,
            close_raw: 10,
            volume_raw: 3,
        }
    }

    /// 正例：同一根 K 线在两个形态下逐格相等。
    #[test]
    fn same_bar_matches_across_the_two_shapes() {
        assert!(ingest_bar_matches_market_bar(
            &ingest(1_000),
            &market(1_000)
        ));
    }

    /// 反例逐列：任何一列（含时间戳）不同都要判不等——这正是手抄映射容易漏掉的那一格。
    #[test]
    fn any_single_column_difference_is_rejected() {
        assert!(!ingest_bar_matches_market_bar(
            &ingest(2_000),
            &market(1_000)
        ));
        let mut close_drifted = ingest(1_000);
        close_drifted.close_raw = 11;
        assert!(!ingest_bar_matches_market_bar(
            &close_drifted,
            &market(1_000)
        ));
        let mut volume_drifted = ingest(1_000);
        volume_drifted.volume_raw = 4;
        assert!(!ingest_bar_matches_market_bar(
            &volume_drifted,
            &market(1_000)
        ));
    }
}
