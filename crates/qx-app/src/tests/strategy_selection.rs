use crate::cases::run_experiment;
use crate::context::{CallerCapability, RunContext};
use crate::error::AppErrorCategory;
use crate::tests::fixtures::{experiment_spec, scratch, write_frame};
use crate::StrategyArtifactIdentity;
use qx_core::{Quantity, Side, SCALE};
use qx_strategy::{
    MarketEvent, Strategy, StrategyContext, StrategyDecision, StrategyOrderIntent,
    STRATEGY_API_VERSION,
};
use std::path::PathBuf;

struct ParameterizedBuy {
    quantity_raw: i128,
    submitted: bool,
}

impl Strategy for ParameterizedBuy {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let intents = if !self.submitted {
            self.submitted = true;
            vec![StrategyOrderIntent {
                intent_id: 1,
                instrument: event
                    .instrument()
                    .cloned()
                    .ok_or_else(|| "event instrument missing".to_string())?,
                side: Side::Buy,
                qty: Quantity::from_raw(self.quantity_raw),
                limit: None,
                policy: None,
                reduce_only: false,
                post_only: false,
            }]
        } else {
            Vec::new()
        };
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

fn research(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

#[test]
fn strategy_selection_experiment_runs_classic_strategies_in_the_shared_rust_kernel() {
    let root = scratch("classic-strategy-selection");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 90);
    let kinds = [
        qx_strategy::BuiltinStrategyKind::SmaCross,
        qx_strategy::BuiltinStrategyKind::EmaCross,
        qx_strategy::BuiltinStrategyKind::Macd,
        qx_strategy::BuiltinStrategyKind::Rsi,
        qx_strategy::BuiltinStrategyKind::Bollinger,
        qx_strategy::BuiltinStrategyKind::DonchianBreakout,
        qx_strategy::BuiltinStrategyKind::Momentum,
        qx_strategy::BuiltinStrategyKind::MeanReversion,
        qx_strategy::BuiltinStrategyKind::Grid,
        qx_strategy::BuiltinStrategyKind::AtrTrend,
    ];
    let mut spec = experiment_spec(
        "classic-strategy-selection",
        &bars,
        &root.join("out"),
        vec![2],
    );
    spec.parameter_space.clear();
    spec.strategy_kinds = kinds.to_vec();
    let result = run_experiment(&spec, &research("classic-strategy-selection"))
        .expect("十种经典策略都应走共享 Rust 回测用例");
    assert_eq!(result.total_candidates, 10);
    assert_eq!(result.succeeded_candidates, 10, "{:?}", result.candidates);
    assert!(result
        .candidates
        .iter()
        .all(|candidate| candidate.error.is_none()));
    assert_eq!(
        result
            .candidates
            .iter()
            .filter_map(|candidate| candidate.strategy_kind.as_deref())
            .collect::<Vec<_>>(),
        [
            "atr_trend",
            "bollinger",
            "donchian_breakout",
            "ema_cross",
            "grid",
            "macd",
            "mean_reversion",
            "momentum",
            "rsi",
            "sma_cross",
        ],
        "候选顺序按策略名稳定排序"
    );
    assert_eq!(result.comparison.as_ref().unwrap().runs.len(), 10);
    assert!(PathBuf::from(&result.artifact_path).is_file());

    let mut reordered = spec.clone();
    reordered.strategy_kinds.reverse();
    assert_eq!(
        run_experiment(&reordered, &research("classic-strategy-selection"))
            .expect("排列不同的同一策略集合应幂等"),
        result,
        "策略选择集合按 kind 规范化后绑定同一实验身份"
    );
    let mut duplicate = spec.clone();
    duplicate.strategy_kinds.push(kinds[0]);
    assert_eq!(
        duplicate.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
    let mut multi_leg = spec.clone();
    multi_leg.strategy_kinds = vec![qx_strategy::BuiltinStrategyKind::PairsArbitrage];
    assert_eq!(
        multi_leg.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
    let mut oos_selection = spec;
    oos_selection.experiment_id = "classic-strategy-selection-oos".into();
    oos_selection.strategy_kinds = kinds[..3].to_vec();
    oos_selection.split_plan = Some(crate::cases::ExperimentSplitPlan {
        train_bars: 20,
        test_bars: 10,
        walk_forward: true,
        step_bars: 10,
    });
    let oos = run_experiment(&oos_selection, &research("classic-strategy-selection-oos"))
        .expect("策略 kind 应参与同一共享样本外选优路径");
    assert_eq!(oos.succeeded_candidates, 3);
    let folds = &oos.out_of_sample.expect("requested OOS report").folds;
    assert_eq!(folds.len(), 7);
    for fold in folds {
        let selected = &oos.candidates[fold.selected_ordinal - 1];
        assert_eq!(fold.selected_strategy_kind, selected.strategy_kind);
        assert!(fold.selected_strategy_kind.is_some());
    }
}

#[test]
fn caller_strategy_parameter_experiment_creates_a_fresh_strategy_per_candidate() {
    let root = scratch("caller-strategy-parameter-experiment");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 60);
    let mut spec = experiment_spec(
        "caller-strategy-parameter-experiment",
        &bars,
        &root.join("out"),
        vec![2],
    );
    spec.parameter_space[0].name = "quantity_raw".into();
    spec.parameter_space[0].values = vec![SCALE, 2 * SCALE];
    let identity = StrategyArtifactIdentity {
        strategy_id: spec.base.strategy.strategy_id.clone(),
        strategy_version: "python-parameterized-v1".into(),
        source_sha256: "a".repeat(64),
    };
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut factory = {
        let seen = seen.clone();
        move |candidate: &crate::BacktestSpec| {
            let quantity_raw = candidate.strategy.quantity_raw;
            seen.lock().unwrap().push(quantity_raw);
            Ok(Box::new(ParameterizedBuy {
                quantity_raw,
                submitted: false,
            }) as Box<dyn Strategy>)
        }
    };
    let result = spec
        .run_with_strategy_factory(
            &identity,
            &mut factory,
            &research("caller-strategy-parameter-experiment"),
        )
        .expect("custom strategies run through every parameter candidate");
    assert_eq!(*seen.lock().unwrap(), vec![SCALE, 2 * SCALE]);
    assert_eq!(result.total_candidates, 2);
    assert_eq!(result.succeeded_candidates, 2, "{:?}", result.candidates);
    assert!(result.candidates.iter().all(|candidate| candidate
        .outcome
        .as_ref()
        .is_some_and(|outcome| outcome.fills > 0)));
    assert_ne!(
        result.candidates[0].outcome.as_ref().unwrap().result_hash,
        result.candidates[1].outcome.as_ref().unwrap().result_hash,
        "parameter values must bind each candidate run identity"
    );

    let mut changed = identity;
    changed.source_sha256 = "b".repeat(64);
    let mut fresh_factory = |_: &crate::BacktestSpec| {
        Ok(Box::new(ParameterizedBuy {
            quantity_raw: SCALE,
            submitted: false,
        }) as Box<dyn Strategy>)
    };
    let conflict = spec
        .run_with_strategy_factory(
            &changed,
            &mut fresh_factory,
            &research("caller-strategy-parameter-experiment"),
        )
        .expect_err("changing strategy source cannot reuse the experiment identity");
    assert_eq!(conflict.category(), AppErrorCategory::Conflict);
}
