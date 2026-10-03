//! 策略订单身份的跨轮口径（V13 R2 第二十五遍 #248）。
//!
//! `intent_id` 只是**轮内**局部序号，而 `client_id` 同时是控制面的 `command_id` 与执行面的
//! `client_order_id`。`invoke_builtin_strategy` 每轮新建一个 `BuiltinStrategy`，序号从 1 重新
//! 起算，所以折进 `round_scope` 之前，第二轮的第一条意图会复用第一轮的身份，被
//! `ControlPlane::submit` 按重复命令拒掉。这里钉住三件事：同轮重放幂等、换轮得到新身份、
//! 连续换轮能被控制面逐条受理。

use super::*;

fn contract_intent(intent_id: u64) -> StrategyContractIntent {
    StrategyContractIntent {
        intent_id,
        instrument: "BTCUSDT.BINANCE".into(),
        side: "buy".into(),
        qty_raw: SCALE,
        limit_price_raw: None,
        reduce_only: false,
        post_only: false,
        position_side: None,
        margin_mode: None,
        position_mode: None,
        leverage: None,
    }
}

fn round_config() -> RuntimeConfig {
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    read_runtime_config(&template).unwrap()
}

fn order_for(config: &RuntimeConfig, round_scope: &str, intent_id: u64) -> Order {
    build_strategy_order_from_contract_intent(
        config,
        "strategy-round",
        7001,
        round_scope,
        &contract_intent(intent_id),
        10,
        0,
    )
    .unwrap()
}

#[test]
fn contract_intent_identity_is_scoped_by_round() {
    let config = round_config();
    // 同一轮重放必须回到同一个身份，否则幂等就没了。
    assert_eq!(
        order_for(&config, "9201", 1).client_id,
        order_for(&config, "9201", 1).client_id
    );
    // 换轮即使轮内序号相同也必须得到新身份。
    assert_ne!(
        order_for(&config, "9201", 1).client_id,
        order_for(&config, "9202", 1).client_id
    );
    // 同一轮内的不同意图仍是不同身份。
    assert_ne!(
        order_for(&config, "9201", 1).client_id,
        order_for(&config, "9201", 2).client_id
    );
    // 摘要可能取到 0，而 client_order_id 必须为正。
    assert!(order_for(&config, "9201", 1).client_id > 0);
    // 归因侧保留策略原值，跨轮折身份不能把 intent_id 洗掉。
    let order = order_for(&config, "9201", 3);
    assert_eq!(
        order.trace.expect("策略订单必须带 trace").intent_id,
        Some(3)
    );
}

#[test]
fn consecutive_rounds_are_accepted_by_the_control_plane() {
    let config = round_config();
    let mut plane = ControlPlane::default();
    let mut accepted = Vec::new();
    for run_id in ["9301", "9302", "9303"] {
        let order = order_for(&config, run_id, 1);
        let command = strategy_submit_command("strategy-round", &order, false, None).unwrap();
        // 这两格都由 client_id 派生：换轮若身份不变，这里会撞 DuplicateCommand。
        assert_eq!(command.command_id, order.client_id);
        assert_eq!(
            command.request_id,
            format!("strategy:strategy-round:{}", order.client_id)
        );
        let record = plane
            .submit(command, 10)
            .unwrap_or_else(|error| panic!("{run_id} 第二轮起的提交被控制面拒绝: {error:?}"));
        accepted.push(record.command_id);
    }
    accepted.sort();
    accepted.dedup();
    assert_eq!(accepted.len(), 3, "三轮必须留下三个不同的订单身份");
}
