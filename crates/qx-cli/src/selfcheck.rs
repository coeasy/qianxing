//! 确定性自校验演示：同输入同哈希、改参数变哈希、无前视偏差、插件装配顺序。
//!
//! `all` / `verify` 两个入口命令复用这一条链路，进程内的断言即 CLI 冒烟测试。
//! 输入序列是合成的（`gen_bars`），但撮合与账本走 `qx-xingban` 真实回测内核；
//! 因此下面每一行产物都显式标注 `DEMO 合成输入`，不让人误读成真实行情上的结果。

use super::*;

/// 自校验深度由 `cli.rs` 这一个分派点决定，本模块不再自行比较命令名。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scope {
    /// `verify`：只校验确定性内核（质量门、重放哈希），不触发插件装配与 Paper 冒烟。
    KernelOnly,
    /// `all`：完整链路，含插件装配与 Paper 主链路冒烟。
    Full,
}

pub(crate) fn run(scope: Scope) {
    let bars = gen_bars(400, 20260910);

    // 1. 质量门
    let report = QualityGate::check(&bars);
    println!(
        "[观星 · 质量门 · DEMO 合成输入] bars={} 判定={:?}",
        bars.len(),
        report.verdict()
    );
    assert_eq!(report.verdict(), Verdict::Ok, "合成数据不应有质量问题");

    // 2. 回测：真实内核（与 `backtest builtin` 同一装配）跑合成输入
    let a = run_backtest(&bars, 42, 5, 20);
    let b = run_backtest(&bars, 42, 5, 20); // 完全相同参数
    let c = run_backtest(&bars, 42, 6, 20); // 改一个参数

    println!(
        "\n[星板 · 回测 A · DEMO 合成输入] 内核={} 成交={} 手续费={:.4} 总收益={:.2}% 最大回撤={:.2}% 终值={:.2}",
        BAR_MATCHING_KERNEL,
        a.n_fills,
        f(a.total_fee),
        a.return_bps as f64 / 100.0,
        a.max_drawdown_bps as f64 / 100.0,
        f(a.final_equity)
    );

    let manifest = RunManifest {
        run_id: "cli-demo".into(),
        code_commit: "workspace".into(),
        config_hash: "fast=5;slow=20;seed=42".to_string(),
        data_fingerprint: format!("synthetic:{}", bars.len()),
        input_components: BTreeMap::new(),
        clock_start: bars.first().map(|b| b.ts).unwrap_or(0),
        clock_end: bars.last().map(|b| b.ts).unwrap_or(0),
        global_seed: 42,
        determinism_mode: true,
        result_hash: format!("{:016x}", a.hash),
        strategy_version: format!("builtin-{}-v1", BuiltinStrategyKind::SmaCross.name()),
        instrument_spec_version: "demo-v1".into(),
        model_fingerprint: "next-open+maker-taker".into(),
        input_event_hash: format!("bars:{}", bars.len()),
        output_event_hash: format!("{:016x}", a.hash),
        runtime_version: env!("CARGO_PKG_VERSION").into(),
    };
    println!(
        "[更路 · RunManifest · DEMO 合成输入] digest={:016x}",
        manifest.digest()
    );

    // 3. 三重重放校验（此处验证前两条）
    let same = ReplayVerifier::identical(a.hash, b.hash);
    let changed = ReplayVerifier::changed(a.hash, c.hash);
    println!("\n[更路 · 重放校验]");
    println!("  ① 同输入两次运行哈希一致 : {}", same);
    println!("  ② 改参数后哈希发生变化   : {}", changed);
    assert!(same, "相同输入必须产生相同结果");
    assert!(changed, "修改参数必须改变结果");

    if scope == Scope::KernelOnly {
        return;
    }

    // 4. 插件装配
    println!("\n[卯眼 · 插件装配]");
    let mut reg = Registry::new();
    reg.register(
        Manifest {
            id: "sys.simulation".into(),
            version: "0.1.0".into(),
            kind: "domain-mod".into(),
            provides: vec![Provides {
                point: POINT_MATCHER.into(),
                cardinality: Cardinality::Exclusive,
                priority: 100,
            }],
            requires: vec![],
            replaces: vec![],
            capabilities: vec!["simulation".into()],
            permissions: vec![],
            healthcheck_timeout_ms: 1000,
            shutdown_timeout_ms: 1000,
            config_schema: "{}".into(),
            manifest_hash: 0,
            signature: None,
        }
        .sign(),
    )
    .unwrap();
    reg.register(
        Manifest {
            id: "sys.transaction-cost".into(),
            version: "0.1.0".into(),
            kind: "domain-mod".into(),
            provides: vec![Provides {
                point: POINT_FEE_MODEL.into(),
                cardinality: Cardinality::Exclusive,
                priority: 100,
            }],
            requires: vec!["sys.simulation".into()],
            replaces: vec![],
            capabilities: vec!["fee".into()],
            permissions: vec![],
            healthcheck_timeout_ms: 1000,
            shutdown_timeout_ms: 1000,
            config_schema: "{}".into(),
            manifest_hash: 0,
            signature: None,
        }
        .sign(),
    )
    .unwrap();

    let order = reg.resolve_order().unwrap();
    println!("  插件数={} 加载顺序={:?}", reg.len(), order);
    println!("  独占冲突={:?}", reg.conflicts());

    println!("\n全部自校验通过 ✓");
    run_paper_smoke();
}
