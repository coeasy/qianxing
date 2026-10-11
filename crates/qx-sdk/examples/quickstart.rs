//! 门面最小上手示例（方案 §6.1「examples」）。
//!
//! `cargo test --workspace` 会**编译**本文件（examples 只构建不运行），所以它不会随着
//! 门面漂移而静默失效。真正跑一遍的版本在 `tests/independent_consumer.rs`。
//!
//! 用法：`cargo run -p qx-sdk --example quickstart -- <bars.json> <输出目录>`

use qx_sdk::{
    BacktestSpec, BuiltinStrategyKind, BuiltinStrategySpec, CallerCapability, Client, DatasetSpec,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let bars = args.next().ok_or("缺少 <bars.json>")?;
    let output_dir = args.next().ok_or("缺少 <输出目录>")?;

    // ① 建会话：能力档 + correlation id。研究档足够跑回测与复核。
    let client = Client::new(CallerCapability::Research, "quickstart-1");

    // ② 先问数据能不能用——这是**裁定**，`usable: false` 是结论不是错误。
    let verdict = client.validate_dataset(&DatasetSpec::new("quickstart", bars.clone()))?;
    println!("数据集 {} 行，可用={}", verdict.rows, verdict.usable);
    if !verdict.usable {
        eprintln!("数据不可用: {:?}", verdict.gaps);
        return Ok(());
    }

    // ③ 同步回测。
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "quickstart-sma");
    strategy.fast_window = 2;
    strategy.slow_window = 3;
    let spec = BacktestSpec::new(
        "quickstart-1",
        "BTCUSDT.BINANCE",
        bars,
        "USDT",
        100_000 * 1_000_000_000,
        20261010,
        output_dir,
        strategy,
    );
    let outcome = client.run_backtest(&spec)?;
    println!(
        "回测完成：{} 个权益点 / {} 笔成交 / hash {}",
        outcome.equity_points, outcome.fills, outcome.result_hash
    );

    // ④ 独立复核产物。被篡改的读数判**失败**而不是报错。
    let verification = client.verify_run(&outcome)?;
    println!(
        "复核 verified={}（不一致 {} 处）",
        verification.verified,
        verification.mismatches.len()
    );
    Ok(())
}
