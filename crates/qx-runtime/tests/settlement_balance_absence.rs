//! 结算币种余额对账的"缺席 vs 算过且为零"口径（V12 §18 交易 TX6）。
//!
//! `RuntimeBalanceDiscrepancy::venue_raw` 之前是裸 `i128`：柜台快照里没有结算币种时，
//! 管线把缺席折算成 0 去和账簿比 —— 账簿恰好也是 0 就报"对上了"，等于替交易所宣称
//! "该币种余额为 0"。这三条用例把两种输入钉成两种结果：报了零 = 无差异，没报 = 一条
//! `venue_raw: None` 的差异，报了非零 = 一条带数的差异。

use qx_core::{AccountBalance, Money};
use qx_runtime::LiveEventPipeline;

fn open_pipeline(tag: &str) -> (std::path::PathBuf, LiveEventPipeline) {
    let root = std::env::temp_dir().join(format!(
        "qianxing-balance-absence-{}-{}-{}",
        std::process::id(),
        tag,
        std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()
    ));
    let pipeline = LiveEventPipeline::open(&root, "binance-main", "USDT").unwrap();
    (root, pipeline)
}

fn balance(asset: &str, free: i64, locked: i64) -> AccountBalance {
    AccountBalance {
        asset: asset.into(),
        free: Money::from_i64(free),
        locked: Money::from_i64(locked),
        borrowed: Money::ZERO,
    }
}

/// 账簿与柜台都是"算过的 0"时确实没有差异 —— 这条是缺席用例的对照组。
#[test]
fn reported_zero_balance_is_not_a_discrepancy() {
    let (root, pipeline) = open_pipeline("reported-zero");
    let discrepancies = pipeline
        .settlement_balance_discrepancies("main", "BINANCE", &[balance("USDT", 0, 0)])
        .unwrap();
    assert!(
        discrepancies.is_empty(),
        "报了零却产出差异: {discrepancies:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 柜台整批快照里没有该币种：账簿同样是 0，但这一轮必须报差异，且柜台侧是缺席。
#[test]
fn missing_settlement_asset_is_reported_absent_not_as_zero() {
    let (root, pipeline) = open_pipeline("absent");
    let discrepancies = pipeline
        .settlement_balance_discrepancies("main", "BINANCE", &[balance("BTC", 1, 0)])
        .unwrap();
    assert_eq!(discrepancies.len(), 1, "缺席被当成 0 与账簿对上了");
    assert_eq!(discrepancies[0].asset, "USDT");
    assert_eq!(discrepancies[0].ledger_raw, 0);
    assert_eq!(discrepancies[0].venue_raw, None, "缺席不得折算成 0");
    let empty = pipeline
        .settlement_balance_discrepancies("main", "BINANCE", &[])
        .unwrap();
    assert_eq!(
        empty, discrepancies,
        "空快照与只报别的币种必须是同一个缺席结果"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 报了非零时柜台侧要带数下来，不能被缺席分支吞掉。
#[test]
fn reported_balance_keeps_the_number() {
    let (root, pipeline) = open_pipeline("reported");
    let discrepancies = pipeline
        .settlement_balance_discrepancies("main", "BINANCE", &[balance("USDT", 12, 3)])
        .unwrap();
    assert_eq!(discrepancies.len(), 1);
    assert_eq!(
        discrepancies[0].venue_raw,
        Some(Money::from_i64(15).raw()),
        "free + locked 才是可对照的柜台余额"
    );
    let _ = std::fs::remove_dir_all(root);
}
