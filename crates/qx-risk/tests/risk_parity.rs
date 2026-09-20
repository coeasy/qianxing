//! 三条风控入口的一致性回归（V9 Phase 1 门禁）。
//!
//! 同一 `OrderRiskContext` 快照 + 同一份 `RuleSet` 配置，必须给出一致的
//! allow/reject 与 violations 集合：
//!
//! (a) `qx_risk::RiskEngine::evaluate_order_with_rules` —— 规范唯一实现；
//! (b) `qx_zhenlu::RiskContext` 门面 —— Paper/实盘 worker 使用的账户快照形状；
//! (c) `qx_zhenlu::RiskGate` + `RuleSet` 包装 —— qx-xingban 回测使用的 deprecated
//!     compat 入口。
//!
//! (b) 是 (a) 的转调，因此逐字段严格相等；(c) 的入口形状不带产品规格与账户限额，
//! 因此只断言规则链可见的部分，并把账户级限额导致的差异显式记录下来。

use qx_core::{
    InstrumentId, MarginMode, Order, OrderPolicy, OrderStatus, Price, Quantity, QxError, Side,
    TradingInstrumentSpec, TradingProduct, SCALE,
};
use qx_risk::{
    MaxNotionalRule, MaxQtyRule, OrderRiskContext, OrderRiskPosition, RiskEngine, RuleSet,
    ORDER_RISK_RULE_SET_VERSION,
};
use qx_zhenlu::{RiskContext, RiskGate};

/// 一次判定所需的全部输入：订单 + 持仓快照 + 两种上下文形状。
struct Fixture {
    order: Order,
    position: OrderRiskPosition,
    /// (b) 门面上下文。
    risk: RiskContext,
    /// (a) 规范上下文，字段与门面逐一对应。
    canonical: OrderRiskContext,
    /// (c) 旧门禁入口的市场参考价。
    reference_price: Option<Price>,
    expected_allowed: bool,
}

struct Case {
    side: Side,
    qty_raw: i128,
    reduce_only: bool,
    available_margin_raw: i128,
    max_position_notional_raw: Option<i128>,
    expected_allowed: bool,
}

fn instrument() -> InstrumentId {
    InstrumentId::parse("BTCUSDT.BINANCE").unwrap()
}

fn perpetual_spec() -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument: instrument(),
        product: TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        max_leverage: 20,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
    }
}

/// 同一份规则集配置：三条入口各自构造一次，配置内容必须完全相同。
fn rule_set() -> RuleSet {
    let mut rules = RuleSet::account_limits_only();
    rules.add(Box::new(MaxQtyRule { max_qty: 2 * SCALE }));
    rules.add(Box::new(MaxNotionalRule {
        max_notional: 150 * SCALE,
    }));
    rules
}

fn fixture(case: &Case) -> Fixture {
    let reference_price = Some(Price::from_raw(100 * SCALE));
    let position = OrderRiskPosition::new(0, 0);
    // 单笔名义额上限放得很宽，让用例只考察“投影持仓名义额”这一条约束；
    // 门面与规范上下文的字段必须逐一对应，否则一致性断言没有意义。
    let max_order_notional_raw = Some(1_000 * SCALE);
    let risk = RiskContext {
        available_margin_raw: Some(case.available_margin_raw),
        reference_price,
        instrument_spec: Some(perpetual_spec()),
        max_order_notional_raw,
        max_position_notional_raw: case.max_position_notional_raw,
    };
    let canonical = OrderRiskContext {
        available_margin_raw: Some(case.available_margin_raw),
        reference_price,
        instrument_spec: Some(perpetual_spec()),
        max_order_notional_raw,
        max_position_notional_raw: case.max_position_notional_raw,
        position,
    };
    let order = Order {
        client_id: 11,
        instrument: instrument(),
        side: case.side,
        qty: Quantity::from_raw(case.qty_raw),
        limit: reference_price,
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: Some(OrderPolicy {
            reduce_only: case.reduce_only,
            position_side: qx_core::PositionSide::Net,
            margin_mode: MarginMode::Cross,
            position_mode: qx_core::PositionMode::OneWay,
            leverage: 10,
            post_only: false,
        }),
    };
    Fixture {
        order,
        position,
        risk,
        canonical,
        reference_price,
        expected_allowed: case.expected_allowed,
    }
}

/// 去掉规则链的 `"RuleName: "` 前缀，使三条入口的 violations 可以逐条比对。
fn normalize(violation: &str) -> String {
    match violation.split_once(": ") {
        Some((prefix, rest)) if !prefix.starts_with('[') => rest.to_string(),
        _ => violation.to_string(),
    }
}

/// (a) 与 (b) 必须逐字段一致：门面只是转调，不允许任何再解释。
fn assert_facade_matches_direct(fixture: &Fixture) {
    let direct =
        RiskEngine::evaluate_order_with_rules(&fixture.canonical, &fixture.order, &rule_set());
    let facade =
        fixture
            .risk
            .evaluate_order_with_rules(&fixture.order, &fixture.position, &rule_set());
    assert_eq!(direct, facade, "门面必须与规范实现逐字段一致");
    assert_eq!(direct.rule_set_version, ORDER_RISK_RULE_SET_VERSION);
    assert_eq!(direct.allowed, fixture.expected_allowed);
    // 规则集版本必须进入决策，否则审计无法回溯当时的配置。
    assert!(!direct.rule_set_version.is_empty());
}

/// (a)/(b) 与 (c) 的 allow/reject 与 violations 集合一致（忽略规则名前缀）。
fn assert_gate_matches_canonical(fixture: &Fixture) {
    let direct =
        RiskEngine::evaluate_order_with_rules(&fixture.canonical, &fixture.order, &rule_set());
    let gate = RiskGate::from_rule_set(rule_set());
    let gate_result =
        gate.check_with_price(&fixture.order, &fixture.position, fixture.reference_price);
    assert_eq!(gate_result.is_ok(), direct.allowed);
    let from_gate: Vec<String> = match gate_result {
        Ok(()) => Vec::new(),
        Err(QxError::BusinessViolation(message)) => message.split("; ").map(normalize).collect(),
        Err(other) => panic!("风控门禁只应返回 BusinessViolation: {other:?}"),
    };
    let from_direct: Vec<String> = direct.violations.iter().map(|v| normalize(v)).collect();
    assert_eq!(from_direct, from_gate);
}

#[test]
fn allow_path_agrees_across_all_three_entries() {
    // 1 张合约 @100：名义额 100 < 150、数量 1 < 2、保证金 10 < 20。
    let fixture = fixture(&Case {
        side: Side::Buy,
        qty_raw: SCALE,
        reduce_only: false,
        available_margin_raw: 20 * SCALE,
        max_position_notional_raw: None,
        expected_allowed: true,
    });
    assert_facade_matches_direct(&fixture);
    assert_gate_matches_canonical(&fixture);
    let direct =
        RiskEngine::evaluate_order_with_rules(&fixture.canonical, &fixture.order, &rule_set());
    assert!(direct.violations.is_empty());
    assert_eq!(direct.projected_position_raw, Some(SCALE));
    assert_eq!(direct.reference_price_raw, Some(100 * SCALE));
}

#[test]
fn notional_violation_agrees_across_all_three_entries() {
    // 2 张合约 @100 → 投影名义额 200 超过规则的 150，三条入口必须同时拒绝，
    // 且拒绝原因逐条一致（名义额折算只有 `OrderRiskContext` 一份实现）。
    let fixture = fixture(&Case {
        side: Side::Buy,
        qty_raw: 2 * SCALE,
        reduce_only: false,
        available_margin_raw: 20 * SCALE,
        max_position_notional_raw: None,
        expected_allowed: false,
    });
    assert_facade_matches_direct(&fixture);
    assert_gate_matches_canonical(&fixture);
    let direct =
        RiskEngine::evaluate_order_with_rules(&fixture.canonical, &fixture.order, &rule_set());
    assert_eq!(direct.violations.len(), 1);
    assert!(direct.violations[0].contains("投影持仓名义额超过账户限额"));
}

#[test]
fn account_level_position_limit_and_rule_express_the_same_constraint() {
    // 同一个投影名义额限额，写成账户级字段或写成规则，判定与原因必须一致。
    let sample = fixture(&Case {
        side: Side::Buy,
        qty_raw: 2 * SCALE,
        reduce_only: false,
        available_margin_raw: 1_000 * SCALE,
        max_position_notional_raw: Some(150 * SCALE),
        expected_allowed: false,
    });
    let as_account_limit = RiskEngine::evaluate_order(&sample.canonical, &sample.order);
    let without_limit = OrderRiskContext {
        max_position_notional_raw: None,
        ..sample.canonical.clone()
    };
    let as_rule = RiskEngine::evaluate_order_with_rules(&without_limit, &sample.order, &rule_set());
    assert_eq!(as_account_limit.allowed, as_rule.allowed);
    assert!(!as_account_limit.allowed);
    let from_account: Vec<String> = as_account_limit
        .violations
        .iter()
        .map(|violation| normalize(violation))
        .collect();
    let from_rule: Vec<String> = as_rule
        .violations
        .iter()
        .map(|violation| normalize(violation))
        .collect();
    assert_eq!(from_account, from_rule);
    assert_eq!(
        as_account_limit.projected_position_raw,
        as_rule.projected_position_raw
    );
    assert_eq!(
        as_account_limit.projected_margin_raw,
        as_rule.projected_margin_raw
    );
}

#[test]
fn reduce_only_invariant_agrees_across_all_three_entries() {
    // reduce-only 不依赖产品规格：即使门禁上下文缺规格也必须成立。
    let fixture = fixture(&Case {
        side: Side::Sell,
        qty_raw: SCALE,
        reduce_only: true,
        available_margin_raw: 20 * SCALE,
        max_position_notional_raw: None,
        expected_allowed: false,
    });
    assert_facade_matches_direct(&fixture);
    assert_gate_matches_canonical(&fixture);
}

#[test]
fn known_gap_account_level_margin_is_invisible_to_legacy_gate() {
    // 现状记录（TODO）：deprecated `RiskGate` 的入口形状只有 `(Order, OrderRiskPosition)`，
    // 看不到可用保证金与产品规格，因此账户级保证金拒绝只在 (a)/(b) 出现。
    // 回测路径接入配置化 `RuleSet` + `OrderRiskContext` 后，本用例应改为三条入口一致。
    let fixture = fixture(&Case {
        side: Side::Buy,
        qty_raw: SCALE,
        reduce_only: false,
        // 1 张合约 @100、10x 杠杆需要 10；只给 5 → 账户级拒绝。
        available_margin_raw: 5 * SCALE,
        max_position_notional_raw: None,
        expected_allowed: false,
    });
    assert_facade_matches_direct(&fixture);
    let direct =
        RiskEngine::evaluate_order_with_rules(&fixture.canonical, &fixture.order, &rule_set());
    assert_eq!(direct.violations.len(), 1);
    assert!(direct.violations[0].contains("订单初始保证金超过账户可用保证金"));
    let gate = RiskGate::from_rule_set(rule_set());
    assert!(
        gate.check_with_price(&fixture.order, &fixture.position, fixture.reference_price)
            .is_ok(),
        "已知差异：旧门禁入口不携带账户级保证金"
    );
}
