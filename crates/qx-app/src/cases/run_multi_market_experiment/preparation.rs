//! Candidate-grid and walk-forward input preparation for multi-market experiments.

use super::*;

pub(super) struct ExperimentPreparation {
    pub(super) target_kind: BuiltinStrategyKind,
    pub(super) active_parameter_space: Vec<ExperimentParameterSpace>,
    pub(super) combinations: Vec<BTreeMap<String, i128>>,
    pub(super) candidate_plans: Vec<CandidatePlan>,
    pub(super) timeline: Option<Vec<u64>>,
    pub(super) folds: Vec<FoldRanges>,
    pub(super) first_training_window: Option<MultiMarketExecutionWindow>,
}

pub(super) fn prepare_experiment(
    spec: &RunMultiMarketExperimentSpec,
    total: usize,
    callback_parameter_search: bool,
) -> Result<ExperimentPreparation, AppError> {
    let target_kind = spec
        .base
        .markets
        .iter()
        .flat_map(|market| &market.strategies)
        .find(|slot| slot.strategy.strategy_id == spec.target_strategy_id)
        .map(|slot| slot.strategy.kind)
        .ok_or_else(|| invariant("validated target strategy slot disappeared"))?;
    let mut kinds = if spec.strategy_kinds.is_empty() {
        vec![target_kind]
    } else {
        spec.strategy_kinds.clone()
    };
    kinds.sort_by_key(|kind| kind.name());
    if callback_parameter_search
        && (!spec.parameter_space.is_empty() || !spec.strategy_kinds.is_empty())
    {
        return Err(invalid(
            "callback_parameter_space 不能与内置参数/策略网格混用",
        ));
    }
    let active_parameter_space = if callback_parameter_search {
        spec.callback_parameter_space.clone()
    } else {
        spec.parameter_space.clone()
    };
    let combinations = if active_parameter_space.is_empty() {
        vec![BTreeMap::new()]
    } else {
        expand_grid(&active_parameter_space)
    };
    let mut candidate_plans = Vec::with_capacity(total);
    if spec.search.is_none() {
        for kind in kinds {
            for parameters in &combinations {
                candidate_plans.push(CandidatePlan {
                    ordinal: candidate_plans.len() + 1,
                    kind,
                    parameters: parameters.clone(),
                    proposal: None,
                    search_index: None,
                });
            }
        }
        if candidate_plans.len() != total {
            return Err(invariant("多市场实验候选计划数量与规格校验数量不一致"));
        }
    }
    let timeline = spec
        .split_plan
        .as_ref()
        .map(|_| load_multi_market_timeline(&spec.base))
        .transpose()?;
    let folds = match (&spec.split_plan, timeline.as_deref()) {
        (Some(plan), Some(timeline)) => experiment_folds(plan, timeline.len())?,
        (None, None) => Vec::new(),
        _ => return Err(invariant("多市场实验切分计划与时间轴状态不一致")),
    };
    if spec.split_plan.is_some() {
        let executions = folds
            .len()
            .checked_mul(total.saturating_add(1))
            .filter(|count| *count <= MAX_MULTI_MARKET_EXPERIMENT_EXECUTIONS)
            .ok_or_else(|| {
                invalid(format!(
                    "多市场实验训练/样本外执行总数超过上限 {MAX_MULTI_MARKET_EXPERIMENT_EXECUTIONS}"
                ))
            })?;
        if executions == 0 {
            return Err(invalid("多市场实验切分未生成训练与样本外执行"));
        }
    }
    let first_training_window = match (timeline.as_deref(), folds.first()) {
        (Some(timeline), Some(fold)) => Some(window_for_range(timeline, fold.train)?),
        (None, None) => None,
        _ => return Err(invariant("多市场实验首折与时间轴状态不一致")),
    };
    Ok(ExperimentPreparation {
        target_kind,
        active_parameter_space,
        combinations,
        candidate_plans,
        timeline,
        folds,
        first_training_window,
    })
}

fn load_multi_market_timeline(spec: &MultiMarketBacktestSpec) -> Result<Vec<u64>, AppError> {
    let mut timeline = BTreeSet::new();
    for market in &spec.markets {
        let payload = read_text(&market.bars_path)?;
        let frame = BarFrame::from_json(&payload).map_err(|error| {
            invalid(format!(
                "市场 {} BarFrame 无效，无法生成实验时间切分: {error:?}",
                market.instrument
            ))
        })?;
        if frame.instrument.to_string() != market.instrument {
            return Err(invalid(format!(
                "市场 {} 的 BarFrame 实际标的是 {}",
                market.instrument, frame.instrument
            )));
        }
        timeline.extend(frame.ts.iter().copied());
    }
    if timeline.len() < crate::MIN_BACKTEST_BARS {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            "多市场实验联合时间轴至少需要两根 Bar",
        ));
    }
    Ok(timeline.into_iter().collect())
}

pub(super) fn window_for_range(
    timeline: &[u64],
    range: [usize; 2],
) -> Result<MultiMarketExecutionWindow, AppError> {
    if range[0] >= range[1] || range[1] > timeline.len() {
        return Err(invariant("多市场实验窗口超出联合时间轴范围"));
    }
    Ok(MultiMarketExecutionWindow {
        trade_start_ts: Some(timeline[range[0]]),
        data_end_ts_exclusive: (range[1] < timeline.len()).then(|| timeline[range[1]]),
    })
}
