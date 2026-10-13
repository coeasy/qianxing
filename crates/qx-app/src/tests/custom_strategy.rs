//! Caller-supplied strategy execution remains on the shared deterministic backtest path.

use crate::cases::{verify_run, StrategyArtifactIdentity};
use crate::context::{CallerCapability, RunContext};
use crate::error::AppErrorCategory;
use crate::spec::{PortfolioBacktestConfig, PortfolioStrategySpec};
use crate::tests::fixtures::{backtest_spec, scratch, write_frame};
use qx_core::{Quantity, Side};
use qx_strategy::{
    MarketEvent, Strategy, StrategyContext, StrategyDecision, StrategyOrderIntent,
    STRATEGY_API_VERSION,
};

struct BuyOnce {
    emitted: bool,
}

impl Strategy for BuyOnce {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let mut intents = Vec::new();
        if !self.emitted {
            let instrument = event
                .instrument()
                .cloned()
                .ok_or_else(|| "expected an instrument market event".to_string())?;
            self.emitted = true;
            intents.push(StrategyOrderIntent {
                intent_id: 1,
                instrument,
                side: Side::Buy,
                qty: Quantity::from_raw(1_000_000),
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,
            });
        }
        Ok(StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("{}:{}", context.strategy_id, event.ts()),
            strategy_id: context.strategy_id.clone(),
            signal_id: event.ts(),
            confidence: 10_000,
            priority: 0,
            expires_at: 0,
            intents,
        })
    }
}

fn identity(sha: &str) -> StrategyArtifactIdentity {
    StrategyArtifactIdentity {
        strategy_id: "app-sma".into(),
        strategy_version: "python-example-1.0.0".into(),
        source_sha256: sha.into(),
    }
}

#[test]
fn injected_strategy_runs_through_shared_replay_and_artifact_chain() {
    let root = scratch("custom-strategy-shared-engine");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let mut spec = backtest_spec("custom-strategy-e2e", &bars, &root.join("out"));
    spec.strategy.fast_window = 5;
    spec.strategy.slow_window = 20;
    let outcome = spec
        .run_with_strategy(
            &identity(&"a".repeat(64)),
            BuyOnce { emitted: false },
            &RunContext::new(CallerCapability::Research, "custom-strategy-e2e"),
        )
        .expect("自定义策略使用共享回测引擎");

    assert!(outcome.fills > 0, "策略订单应被共享撮合链处理");
    let verification = verify_run(
        &outcome,
        &RunContext::new(CallerCapability::Research, "custom-strategy-e2e"),
    )
    .expect("回放校验");
    assert!(verification.verified, "共享回放校验必须通过");
    let summary: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&outcome.artifacts.summary).unwrap())
            .unwrap();
    assert_eq!(summary["strategy_version"], "python-example-1.0.0");
}

#[test]
fn injected_strategy_can_share_a_portfolio_ledger_with_builtin_strategies() {
    let root = scratch("custom-strategy-portfolio");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 90);
    let mut spec = backtest_spec("custom-strategy-portfolio", &bars, &root.join("out"));
    spec.strategy = crate::spec::BuiltinStrategySpec::new(
        spec.strategy.kind,
        spec.strategy.strategy_id.clone(),
    );
    let cash = spec.initial_cash_raw;
    spec.portfolio = Some(PortfolioBacktestConfig {
        primary_priority: 0,
        primary_reserved_capital_raw: cash / 2,
        additional_strategies: vec![PortfolioStrategySpec {
            priority: 1,
            reserved_capital_raw: cash / 2,
            strategy: crate::spec::BuiltinStrategySpec {
                kind: qx_strategy::BuiltinStrategyKind::Momentum,
                strategy_id: "builtin-secondary".into(),
                ..spec.strategy.clone()
            },
        }],
    });
    let context = RunContext::new(CallerCapability::Research, "custom-strategy-portfolio");
    let outcome = spec
        .run_with_strategy(
            &identity(&"c".repeat(64)),
            BuyOnce { emitted: false },
            &context,
        )
        .expect("自定义策略与内置策略共享组合账簿");

    assert!(outcome.fills > 0);
    let fills = std::fs::read_to_string(&outcome.artifacts.fills).expect("成交归因产物");
    assert!(
        fills.lines().any(|line| line.contains("app-sma")),
        "主策略成交归因"
    );
    assert!(
        fills.lines().any(|line| line.contains("builtin-secondary")),
        "附属内置策略成交归因"
    );
    assert!(verify_run(&outcome, &context).expect("重放复核").verified);
}

#[test]
fn injected_strategy_source_digest_is_part_of_run_identity() {
    let root = scratch("custom-strategy-identity");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let mut spec = backtest_spec("custom-strategy-identity", &bars, &root.join("out"));
    spec.strategy.fast_window = 5;
    spec.strategy.slow_window = 20;
    let context = RunContext::new(CallerCapability::Research, "custom-strategy-identity");
    spec.run_with_strategy(
        &identity(&"a".repeat(64)),
        BuyOnce { emitted: false },
        &context,
    )
    .expect("首次执行");

    let error = spec
        .run_with_strategy(
            &identity(&"b".repeat(64)),
            BuyOnce { emitted: false },
            &context,
        )
        .expect_err("不同策略源码不能复用既有 run_id");
    assert_eq!(error.category(), AppErrorCategory::Conflict);
}

#[test]
fn injected_strategy_rejects_invalid_source_digest_before_execution() {
    let root = scratch("custom-strategy-invalid-identity");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let mut spec = backtest_spec("custom-strategy-invalid", &bars, &root.join("out"));
    spec.strategy.fast_window = 5;
    spec.strategy.slow_window = 20;
    let mut bad_identity = identity(&"A".repeat(64));
    bad_identity.strategy_id = "other-strategy".into();
    let error = spec
        .run_with_strategy(
            &bad_identity,
            BuyOnce { emitted: false },
            &RunContext::new(CallerCapability::Research, "custom-strategy-invalid"),
        )
        .expect_err("非法且不匹配的策略身份需在执行前拒绝");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);

    let mut tuned_spec = spec.clone();
    tuned_spec.strategy.fast_window = 2;
    let error = tuned_spec
        .run_with_strategy(
            &identity(&"a".repeat(64)),
            BuyOnce { emitted: false },
            &RunContext::new(CallerCapability::Research, "custom-strategy-invalid"),
        )
        .expect_err("内置策略旋钮不能静默作用于外部回调");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
}
