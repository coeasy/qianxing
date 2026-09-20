//! P1a 概念单点化 · 风控门禁构造单点（V10 §4.5 / §6 P1a）。
//!
//! 反向验证：只要"零规则即默认放行"的门禁形状回来（`RiskGate::new()` /
//! `RiskGate::default()` / `RuleSet::new()` / `RuleSet::default()`），
//! `no_bare_risk_gate_construction_survives_in_the_tree` 即红；把演示路径的
//! 门禁换回空规则集，`conservative_default_*` 用例即红。

use qx_core::{EventKind, InstrumentId, Money, Order, OrderStatus, Quantity, Side};
use qx_guanxing::Bar;
use qx_risk::{RuleSet, CONSERVATIVE_DEFAULT_RULE_SET_VERSION, CONSERVATIVE_MAX_QTY_RAW};
use qx_xingban::{
    BacktestConfig, BacktestEngine, BarStrategy, DataTier, MakerTakerFeeModel,
    NextBarOpenFillModel, NoMargin, VirtualTradingConfig, ZeroLatency,
};
use qx_zhenlu::RiskGate;
use std::path::{Path, PathBuf};

struct BuyBig(i128);

impl BarStrategy for BuyBig {
    fn on_bar(
        &mut self,
        _history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        position: i128,
    ) -> Option<Order> {
        if position != 0 {
            return None;
        }
        let qty = std::mem::replace(&mut self.0, 0);
        if qty == 0 {
            return None;
        }
        Some(mk_order(qty, instrument))
    }
}

fn mk_order(qty_raw: i128, instrument: &InstrumentId) -> Order {
    Order {
        client_id: 0,
        instrument: instrument.clone(),
        side: Side::Buy,
        qty: Quantity::from_raw(qty_raw),
        limit: None,
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
    }
}

fn config_with(gate: RiskGate) -> BacktestConfig {
    BacktestConfig {
        instrument: InstrumentId::parse("T.SIM").unwrap(),
        instrument_spec: None,
        account_id: "main".into(),
        currency: "USD".into(),
        initial_cash: Money::from_i64(1_000_000),
        multiplier: 1,
        fill: Box::new(NextBarOpenFillModel),
        fee: Box::new(MakerTakerFeeModel {
            maker_bp: 0,
            taker_bp: 0,
        }),
        data_tier: DataTier::Bar,
        latency: Box::new(ZeroLatency),
        margin: Box::new(NoMargin),
        seed: 3,
        risk: gate,
        virtual_trading: VirtualTradingConfig::default(),
    }
}

/// 演示 / 缺省回落路径唯一允许的门禁形状：具名保守默认，超限即拒。
#[test]
fn conservative_default_gate_is_named_and_fails_closed_on_oversized_orders() {
    let gate = RiskGate::conservative_default();
    assert_eq!(
        gate.rule_set().version(),
        CONSERVATIVE_DEFAULT_RULE_SET_VERSION,
        "回落门禁必须携带具名规则集版本，便于回测摘要与执行审计回溯"
    );
    assert!(
        gate.rule_set().rule_count() > 0,
        "具名保守默认不允许是零规则集"
    );

    let bars = vec![
        Bar::new(1, 100, 101, 99, 100, 1_000_000),
        Bar::new(2, 101, 102, 100, 101, 1_000_000),
    ];

    // 门禁本身：超限订单必须被具名规则拒绝，上限内放行。
    let instrument = InstrumentId::parse("T.SIM").unwrap();
    let oversized = CONSERVATIVE_MAX_QTY_RAW + Quantity::from_i64(1).raw();
    let violation = gate
        .check(
            &mk_order(oversized, &instrument),
            &qx_risk::OrderRiskPosition::new(0, 0),
        )
        .expect_err("超过保守上限的订单必须被拒绝");
    assert!(
        format!("{violation:?}").contains("MaxQty"),
        "拒绝原因必须来自保守默认规则集，实际: {violation:?}"
    );
    gate.check(
        &mk_order(Quantity::from_i64(1).raw(), &instrument),
        &qx_risk::OrderRiskPosition::new(0, 0),
    )
    .expect("上限内的订单不应被回落门禁拒绝");

    // 引擎侧：演示路径不会因为"没有规则"而静默成交超限订单。
    let oversized_run = match BacktestEngine::new(config_with(RiskGate::conservative_default()))
        .run(&bars, &mut BuyBig(oversized))
    {
        Ok(report) => report,
        Err(error) => panic!("回测演示路径应当跑通并记录拒绝: {error:?}"),
    };
    assert!(
        oversized_run.fills.is_empty(),
        "超限订单不得成交，实际 {} 笔",
        oversized_run.fills.len()
    );
    let reasons: Vec<String> = oversized_run
        .event_log
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::Rejected { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .collect();
    assert!(
        reasons.iter().any(|reason| reason.contains("MaxQty")),
        "拒绝原因必须来自具名保守默认规则集，实际: {reasons:?}"
    );

    // 控制组（反向验证锚点）：把门禁换成"故意零静态规则"的形状，同一笔超限订单
    // 会静默成交——说明上面的断言检查的是保守默认规则集，而不是引擎本身在拒绝。
    let control = match BacktestEngine::new(config_with(RiskGate::from_rule_set(
        RuleSet::account_limits_only(),
    )))
    .run(&bars, &mut BuyBig(oversized))
    {
        Ok(report) => report,
        Err(error) => panic!("控制组必须跑通: {error:?}"),
    };
    assert_eq!(
        control.fills.len(),
        1,
        "控制组失效：零静态规则的门禁也必须能成交，否则本用例没有判别力"
    );

    let within = CONSERVATIVE_MAX_QTY_RAW - Quantity::from_i64(1).raw();
    let report = match BacktestEngine::new(config_with(RiskGate::conservative_default()))
        .run(&bars, &mut BuyBig(within))
    {
        Ok(report) => report,
        Err(error) => panic!("保守上限以内的回测演示路径必须跑通: {error:?}"),
    };
    assert_eq!(report.fills.len(), 1);
}

fn workspace_root() -> PathBuf {
    let mut dir = std::env::current_dir().expect("cwd");
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file()
            && std::fs::read_to_string(&manifest)
                .unwrap_or_default()
                .contains("[workspace]")
        {
            return dir;
        }
        if !dir.pop() {
            panic!("未找到 workspace 根目录");
        }
    }
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// 架构不变量：`RiskGate` / `RuleSet` 只剩具名构造器，任何"零规则即默认形状"
/// 的构造点（含 `Default` 实现）都不允许存在。
#[test]
fn no_bare_risk_gate_construction_survives_in_the_tree() {
    let root = workspace_root();
    // 模式由片段拼装，避免本用例自身成为扫描命中点。
    let forbidden: Vec<String> = ["RiskGate", "RuleSet"]
        .iter()
        .flat_map(|ty| {
            [
                format!("{ty}::new("),
                format!("{ty}::default("),
                format!("impl Default for {ty}"),
            ]
        })
        .collect();
    let mut offenders = Vec::new();
    let mut files = Vec::new();
    collect_rs_files(&root.join("crates"), &mut files);
    assert!(files.len() > 50, "扫描范围异常");
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        for (index, line) in text.lines().enumerate() {
            // 文档/注释里描述历史行为是允许的，只禁止真实构造点。
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if forbidden.iter().any(|pattern| line.contains(pattern)) {
                offenders.push(format!(
                    "{}:{}: {}",
                    file.strip_prefix(&root).unwrap_or(file.as_path()).display(),
                    index + 1,
                    trimmed
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "风控门禁必须经由唯一具名构造器（RiskGate::from_rule_set / \
         RiskGate::conservative_default / RuleSet::with_version / \
         RuleSet::account_limits_only / RuleSet::conservative_default）；违规位置:\n{}",
        offenders.join("\n")
    );
}
