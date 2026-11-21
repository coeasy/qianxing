//! 门面最小上手示例（方案 §6.1「examples」）。
//!
//! `cargo test --workspace` 会**编译**本文件（examples 只构建不运行），所以它不会随着
//! 门面漂移而静默失效。真正跑一遍的版本在 `tests/independent_consumer.rs`。
//!
//! 用法：`cargo run -p qx-sdk --example quickstart -- <bars.json> <输出目录>`

use qx_sdk::{
    BacktestDatasetInput, BacktestPreflightCheckStatusV1, BacktestSpec, BuiltinStrategyKind,
    BuiltinStrategySpec, CallerCapability, Client,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let bars = args.next().ok_or("缺少 <bars.json>")?;
    let output_dir = args.next().ok_or("缺少 <输出目录>")?;

    // ① 建会话：能力档 + correlation id。研究档足够跑回测与复核。
    let client = Client::new(CallerCapability::Research, "quickstart-1");

    // ② 声明完整规格后做统一只读预检；标的、样本量、规则有效期一并提前反馈。
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "quickstart-sma");
    strategy.fast_window = 2;
    strategy.slow_window = 3;
    let spec = BacktestSpec::new(
        BacktestDatasetInput {
            run_id: "quickstart-1".into(),
            instrument: "BTCUSDT.BINANCE".into(),
            bars_path: bars,
        },
        "USDT",
        100_000 * 1_000_000_000,
        20261010,
        output_dir,
        strategy,
    );
    let readiness = client.preflight_backtest(&spec)?;
    println!(
        "预检 ready={}：{} 根 Bar，数据标的 {}",
        readiness.ready, readiness.data.rows, readiness.data.instrument
    );
    if !readiness.ready {
        let reasons = readiness
            .checks
            .iter()
            .filter(|check| check.status == BacktestPreflightCheckStatusV1::Blocked)
            .map(|check| {
                format!(
                    "{}: {}",
                    check.id,
                    check.detail.as_deref().unwrap_or("未通过")
                )
            })
            .collect::<Vec<_>>();
        return Err(format!("回测预检阻断: {}", reasons.join("；")).into());
    }

    // ③ 同步回测；运行时仍会重新读取并校验数据。
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
    if !verification.verified {
        return Err(format!("回测产物复核失败: {:?}", verification.mismatches).into());
    }
    Ok(())
}
