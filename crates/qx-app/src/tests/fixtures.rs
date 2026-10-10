//! 用例共用的夹具：合成 BarFrame、临时目录、一份能跑出成交的回测规格。
//!
//! 价格路径刻意做成**振荡**（涨 10 根、跌 10 根循环）而不是单调序列：单调序列上任何
//! 均线交叉策略都不会产生交叉点，"跑通"就退化成"跑了个空"，成交数、权益曲线与
//! 结果哈希三样都验不出东西。

use crate::spec::{BacktestSpec, BuiltinStrategySpec, DatasetSpec};
use qx_strategy::BuiltinStrategyKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// 定点标度（与 `qx_core::SCALE` 同值；这里写死是为了让夹具不依赖被测代码的常量）。
const SCALE: i128 = 1_000_000_000;

/// 一次测试独享的临时目录。名字带 pid 与自增序号，所以并行跑的用例不会互相踩。
pub(super) fn scratch(label: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let serial = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "qx-app-tests/{label}-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("建临时目录");
    root
}

/// 合成一份 BarFrame JSON。`bars` 根，价格按 20 根一周期振荡。
pub(super) fn frame_json(instrument: &str, bars: usize) -> String {
    let mut ts = Vec::new();
    let mut open = Vec::new();
    let mut high = Vec::new();
    let mut low = Vec::new();
    let mut close = Vec::new();
    let mut volume = Vec::new();
    for index in 0..bars {
        let phase = (index % 20) as i128;
        let price = if phase < 10 {
            100 + phase * 2
        } else {
            100 + (20 - phase) * 2
        };
        ts.push(1_000 + (index as u64) * 1_000);
        open.push(price * SCALE);
        high.push((price + 1) * SCALE);
        low.push((price - 1) * SCALE);
        close.push(price * SCALE);
        volume.push(SCALE);
    }
    serde_json::json!({
        "schema_version": 1,
        "instrument": instrument,
        "source": "qx-app-test-v1",
        "ts": ts,
        "open_raw": open,
        "high_raw": high,
        "low_raw": low,
        "close_raw": close,
        "volume_raw": volume,
    })
    .to_string()
}

/// 把一份 BarFrame 落到临时目录里，返回路径。
pub(super) fn write_frame(root: &Path, name: &str, instrument: &str, bars: usize) -> String {
    let path = root.join(name);
    std::fs::write(&path, frame_json(instrument, bars)).expect("写 BarFrame 夹具");
    path.to_string_lossy().into_owned()
}

/// 一份能跑出成交的回测规格。窗口取 2/3：振荡周期 20 根，短窗口才抓得住交叉。
pub(super) fn backtest_spec(run_id: &str, bars_path: &str, output_dir: &Path) -> BacktestSpec {
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "app-sma");
    strategy.fast_window = 2;
    strategy.slow_window = 3;
    BacktestSpec::new(
        run_id,
        "BTCUSDT.BINANCE",
        bars_path,
        "USDT",
        100_000 * SCALE,
        20261010,
        output_dir.to_string_lossy().into_owned(),
        strategy,
    )
}

/// 一份数据集规格。
pub(super) fn dataset_spec(dataset_id: &str, bars_path: &str) -> DatasetSpec {
    DatasetSpec::new(dataset_id, bars_path)
}
