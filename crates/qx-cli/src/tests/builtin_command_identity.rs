//! 策略订单身份映射的行为用例（V11 R5-3）。
//!
//! 契约口径写在 `schemas/strategy_api_v1.md`："an `intent_id` is unique within one output;
//! the runtime checks uniqueness per decision, not per account" —— 也就是说 `intent_id` 是
//! **一次输出内的局部序号**，内置策略每轮从 1 重新计数完全合法（`BuiltinStrategy::new` 重置
//! `next_intent_id`）。但映射点把它原样写成 `Order.client_id`，而 `client_id` 两侧都是全店
//! 唯一键：控制面的 `command_id`（`qx-control` 的 `DuplicateCommand`）与执行面的
//! `client_order_id`（`qx-runtime/pipeline.rs` 的"重复的 live client_order_id"）。
//!
//! 于是 Strategy 链上两条腿都走不通：新一轮与上一轮同尺寸 → 被当成旧命令的结果静默吞掉
//! （信号丢失，不下单）；不同尺寸 → 撞上 command_id 硬错（作业失败）。四颗用例分别锁住
//! 映射的四个方向：换一轮同尺寸要能下单、换一轮换尺寸要能下单、重放同一轮要仍然幂等、
//! 同一轮里的多条腿要互不相撞。

use super::*;

const STRATEGY_ID: &str = "strategy-builtin";

/// 跑一轮内置策略，返回它与 Strategy worker 完全同形的契约输出。
///
/// `run_id` 走的是 worker 的真实传法：`workers.rs` 把 `queued.run.run_id` 当 request_id 传入，
/// `strategy_host.rs` 再把它写回 `output.request_id`。"哪一轮"正是身份映射要看的信息。
fn builtin_round(
    run_id: u64,
    closes: &[(u64, i128)],
    quantity: i64,
) -> qx_runtime::StrategyContractOutput {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let base = qx_strategy::BuiltinStrategyConfig::new(
        qx_strategy::BuiltinStrategyKind::Momentum,
        STRATEGY_ID,
        instrument.clone(),
        Quantity::from_i64(quantity),
    )
    .unwrap();
    let mut strategy = qx_strategy::BuiltinStrategy::new(qx_strategy::BuiltinStrategyConfig {
        period: 2,
        threshold_bps: 10,
        ..base
    })
    .unwrap();
    let mut context = qx_strategy::StrategyContext {
        strategy_id: STRATEGY_ID.into(),
        strategy_version: "momentum-v1".into(),
        account_id: "main".into(),
        venue_id: "paper".into(),
        data_fingerprint: "bars-v1".into(),
        as_of: 1,
        positions: BTreeMap::new(),
        cash: BTreeMap::new(),
        available_margin_raw: Some(1_000_000),
        risk_state: "ready".into(),
    };
    let mut decision = None;
    for (ts, close) in closes {
        context.as_of = *ts;
        let output = strategy
            .on_event(
                &context,
                &qx_strategy::MarketEvent::Bar {
                    instrument: instrument.clone(),
                    ts: *ts,
                    open_raw: *close,
                    high_raw: *close,
                    low_raw: *close,
                    close_raw: *close,
                    volume_raw: 1,
                },
            )
            .unwrap();
        if !output.intents.is_empty() {
            decision = Some(output);
        }
    }
    let mut output = qx_runtime::StrategyContractOutput::from_native_decision(
        &decision.expect("轮次必须出信号"),
    )
    .unwrap();
    output.request_id = run_id.to_string();
    output
}

/// 一直上涨的行情：每一轮的第一个信号都是同一条 `Buy` 意图、策略侧序号 1。
fn rising_round(run_id: u64, quantity: i64) -> qx_runtime::StrategyContractOutput {
    builtin_round(run_id, &[(1, 100), (2, 100), (3, 101)], quantity)
}

fn builtin_runtime() -> RuntimeConfig {
    let config = read_runtime_config(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy")
            .join("qianxing.runtime.builtin-strategy.example.json"),
    )
    .unwrap();
    assert_eq!(config.strategy.id.as_deref(), Some(STRATEGY_ID));
    config
}

/// Strategy worker 的 intents 分支：契约输出 + 一条意图 → 订单 → 控制命令，一步不绕。
fn command_for_intent(
    config: &RuntimeConfig,
    output: &qx_runtime::StrategyContractOutput,
    index: usize,
    current_qty: i128,
    now: u64,
) -> ControlCommand {
    let intent = &output.intents[index];
    let order = build_strategy_order_from_contract_intent(
        config,
        STRATEGY_ID,
        output.signal_id,
        &output.request_id,
        intent,
        now,
        current_qty,
    )
    .unwrap();
    assert_eq!(
        order.trace.as_ref().and_then(|trace| trace.intent_id),
        Some(intent.intent_id),
        "归因必须留住策略侧的原始 intent_id"
    );
    strategy_submit_command(STRATEGY_ID, &order, false, None).unwrap()
}

/// 同一轮里换掉策略侧序号，得到一个"只有意图身份不同"的兄弟输出（多腿就是这种形状）。
fn round_with_second_leg(
    output: &qx_runtime::StrategyContractOutput,
) -> qx_runtime::StrategyContractOutput {
    let mut multi = output.clone();
    let mut leg = multi.intents[0].clone();
    leg.intent_id += 1;
    multi.intents.push(leg);
    multi
}

struct Fixture {
    root: PathBuf,
    config: RuntimeConfig,
    store: ControlStateBackend,
    queue: ControlCommandQueue,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = temp_cli_case_dir(label);
        Self {
            config: builtin_runtime(),
            store: ControlStateBackend::Files(JsonStateStore::new(root.clone())),
            queue: ControlCommandQueue::new(root.join("control-queue")),
            root,
        }
    }

    /// 走一遍 worker 的受理 + 终态回写。
    fn submit_and_execute(&self, command: &ControlCommand, accept_ts: u64, final_ts: u64) {
        assert_eq!(
            persist_strategy_submit(&self.store, &self.queue, command, accept_ts).unwrap(),
            "ORDER_INTENT_ACCEPTED"
        );
        self.store
            .transact(|plane| {
                plane.execute(command.command_id, final_ts, |_| {
                    Ok("PAPER_SUBMITTED".into())
                })
            })
            .map(|(_, record)| record)
            .unwrap()
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_later_round_with_the_same_intent_still_places_its_order() {
    let first_round = rising_round(4101, 1);
    let fixture = Fixture::new("builtin-command-identity-same");
    // 前提：两轮都是各自那一轮的第一条意图，策略侧序号相同是契约允许的形状，不是要改的东西。
    let later_round = rising_round(4102, 1);
    assert_eq!(
        first_round.intents[0].intent_id,
        later_round.intents[0].intent_id
    );
    assert_eq!(first_round.intents[0].side, later_round.intents[0].side);

    let first = command_for_intent(&fixture.config, &first_round, 0, 0, 900);
    fixture.submit_and_execute(&first, 901, 902);

    // 平仓后策略再次进场：这是新一轮的合法订单，不能被上一轮的执行结果顶掉。
    let later = command_for_intent(&fixture.config, &later_round, 0, 0, 903);
    assert_ne!(
        later.command_id, first.command_id,
        "另一轮信号复用了上一轮的订单身份"
    );
    assert_eq!(
        persist_strategy_submit(&fixture.store, &fixture.queue, &later, 904).unwrap(),
        "ORDER_INTENT_ACCEPTED",
        "同尺寸的第二轮信号被当成旧命令的执行结果静默吞掉，这一单从来没有下出去"
    );
}

#[test]
fn a_later_round_with_a_different_intent_is_not_a_command_collision() {
    let first_round = rising_round(4401, 1);
    let fixture = Fixture::new("builtin-command-identity-different");
    let bigger_round = rising_round(4402, 2);

    let first = command_for_intent(&fixture.config, &first_round, 0, 0, 910);
    fixture.submit_and_execute(&first, 911, 912);

    // 加仓（网格按档递进）换的是尺寸，不是轮次身份：撞 command_id 就是链路断裂。
    let bigger = command_for_intent(&fixture.config, &bigger_round, 0, 1, 913);
    assert_ne!(bigger.command_id, first.command_id);
    assert_eq!(
        persist_strategy_submit(&fixture.store, &fixture.queue, &bigger, 914).unwrap(),
        "ORDER_INTENT_ACCEPTED",
        "第二轮信号撞上一轮的 command_id，Strategy 作业在这里报错中断"
    );
}

#[test]
fn replaying_the_same_round_stays_idempotent() {
    let fixture = Fixture::new("builtin-command-replay");
    let round = rising_round(4201, 1);
    let command = command_for_intent(&fixture.config, &round, 0, 0, 920);
    fixture.submit_and_execute(&command, 921, 922);

    // 同一个 run 重跑（同一批行情）必须仍然落回旧命令的结果，否则重启一次就多发一单。
    let replay = command_for_intent(&fixture.config, &rising_round(4201, 1), 0, 0, 923);
    assert_eq!(
        replay.command_id, command.command_id,
        "同一轮重放换出了新身份"
    );
    assert_eq!(
        persist_strategy_submit(&fixture.store, &fixture.queue, &replay, 924).unwrap(),
        "ORDER_INTENT_ALREADY_EXECUTED"
    );
}

#[test]
fn intents_of_one_round_keep_distinct_command_identities() {
    let multi = round_with_second_leg(&rising_round(4301, 1));
    let config = builtin_runtime();
    let first = command_for_intent(&config, &multi, 0, 0, 930);
    let second = command_for_intent(&config, &multi, 1, 0, 930);
    assert_ne!(
        first.command_id, second.command_id,
        "同一轮里的两条腿撞在同一个 command_id 上"
    );
    assert_ne!(first.request_id, second.request_id);
}
