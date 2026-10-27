//! Deterministic startup-history sensitivity checks for native `Strategy` implementations.

use crate::{MarketEvent, Strategy, StrategyContext, StrategyDecision};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupWindowObservation {
    #[serde(rename = "requested_events", alias = "requested_bars")]
    pub requested_bars: usize,
    #[serde(rename = "available_events", alias = "available_bars")]
    pub available_bars: usize,
    pub history_truncated: bool,
    pub decision: StrategyDecision,
    pub changed_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intermediate_state: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_intermediate_fields: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupDiagnostics {
    pub status: String,
    #[serde(rename = "reference_window_events", alias = "reference_window_bars")]
    pub reference_window_bars: usize,
    pub decision_timestamp: u64,
    pub windows: Vec<StartupWindowObservation>,
    pub interpretation: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupTrajectoryDivergence {
    pub decision_timestamp: u64,
    #[serde(rename = "requested_events", alias = "requested_bars")]
    pub requested_bars: usize,
    #[serde(rename = "reference_window_events", alias = "reference_window_bars")]
    pub reference_window_bars: usize,
    pub changed_fields: Vec<String>,
    pub changed_intermediate_fields: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupTrajectoryDiagnostics {
    pub status: String,
    pub decision_points_available: usize,
    pub decision_points_checked: usize,
    pub decision_points_sampled: bool,
    #[serde(default)]
    pub decision_points_eligible: usize,
    #[serde(default)]
    pub decision_points_excluded_insufficient_reference_history: usize,
    pub strategy_replays: usize,
    #[serde(rename = "changed_windows_events", alias = "changed_windows_bars")]
    pub changed_windows_bars: Vec<usize>,
    #[serde(
        rename = "changed_fields_by_window_events",
        alias = "changed_fields_by_window"
    )]
    pub changed_fields_by_window: std::collections::BTreeMap<usize, Vec<String>>,
    #[serde(
        rename = "intermediate_state_changed_windows_events",
        alias = "intermediate_state_changed_windows_bars"
    )]
    pub intermediate_state_changed_windows_bars: Vec<usize>,
    #[serde(
        rename = "intermediate_state_changed_fields_by_window_events",
        alias = "intermediate_state_changed_fields_by_window"
    )]
    pub intermediate_state_changed_fields_by_window: std::collections::BTreeMap<usize, Vec<String>>,
    #[serde(default)]
    pub divergences: Vec<StartupTrajectoryDivergence>,
    pub first_divergence: Option<StartupTrajectoryDivergence>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuturePerturbationCheck {
    pub perturbed_event_timestamp: u64,
    pub perturbation_kind: String,
    pub price_shift_raw: i128,
    pub volume_shift_raw: i128,
    pub prior_decision_points_checked: usize,
    pub changed_decision_points: usize,
    pub changed_decision_fields: Vec<String>,
    pub changed_intermediate_fields: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FuturePerturbationDiagnostics {
    pub status: String,
    pub decision_points_available: usize,
    pub perturbation_targets_available: usize,
    pub perturbation_targets_checked: usize,
    pub decision_points_checked: usize,
    pub strategy_replays: usize,
    pub decision_points_sampled: bool,
    pub checks: Vec<FuturePerturbationCheck>,
    pub reason: String,
    pub interpretation: String,
}

/// Replays a complete event stream after changing selected future Bar inputs and confirms that
/// decisions already emitted before each changed event remain stable. This is a bounded
/// black-box causality check; it cannot establish how a strategy treats data it fetches itself.
pub fn diagnose_future_perturbations<F>(
    context: &StrategyContext,
    events: &[MarketEvent],
    max_checks: usize,
    mut factory: F,
) -> Result<FuturePerturbationDiagnostics, String>
where
    F: FnMut() -> Result<Box<dyn Strategy>, String>,
{
    const MAX_FUTURE_PERTURBATION_CHECKS: usize = 64;
    const MAX_PERTURBATION_REPLAY_EVENTS: usize = 1_000_000;
    const MAX_PERTURBATION_TRACE_EVENTS: usize = 65_536;
    if max_checks == 0 || max_checks > MAX_FUTURE_PERTURBATION_CHECKS {
        return Err(format!(
            "future perturbation check budget must be in 1..={MAX_FUTURE_PERTURBATION_CHECKS}"
        ));
    }
    if events.is_empty() {
        return Err("future perturbation diagnostics require at least one event".into());
    }
    for event in events {
        event.validate()?;
    }
    if events.windows(2).any(|pair| pair[0].ts() >= pair[1].ts()) {
        return Err("future perturbation events must be strictly increasing by timestamp".into());
    }
    if events
        .iter()
        .any(|event| !matches!(event, MarketEvent::Bar { .. }))
    {
        return Err("future perturbation currently requires Bar events".into());
    }
    let targets_available = events.len().saturating_sub(1);
    if targets_available == 0 {
        return Ok(FuturePerturbationDiagnostics {
            status: "insufficient_history".into(),
            decision_points_available: events.len(),
            perturbation_targets_available: 0,
            perturbation_targets_checked: 0,
            decision_points_checked: 0,
            strategy_replays: 0,
            decision_points_sampled: false,
            checks: Vec::new(),
            reason: "future perturbation requires at least two events".into(),
            interpretation: "No future event exists after a decision point, so a future perturbation comparison cannot be formed.".into(),
        });
    }

    if events.len() > MAX_PERTURBATION_TRACE_EVENTS {
        return Ok(FuturePerturbationDiagnostics {
            status: "insufficient_work_budget".into(),
            decision_points_available: events.len(),
            perturbation_targets_available: targets_available,
            perturbation_targets_checked: 0,
            decision_points_checked: 0,
            strategy_replays: 0,
            decision_points_sampled: true,
            checks: Vec::new(),
            reason: format!(
                "event count exceeds the memory-bounded diagnostic trace limit of {MAX_PERTURBATION_TRACE_EVENTS}"
            ),
            interpretation: "No future perturbation was executed because retaining a comparable trace would exceed the configured memory bound.".into(),
        });
    }

    let baseline = replay_decision_trace(context, events, &mut factory)?;
    let affordable_checks =
        (MAX_PERTURBATION_REPLAY_EVENTS / events.len().max(1)).saturating_sub(1);
    let checks_limit = max_checks.min(affordable_checks);
    let selected_targets = evenly_spaced_indices(targets_available, checks_limit)
        .into_iter()
        .map(|offset| offset + 1)
        .collect::<Vec<_>>();
    let mut result = FuturePerturbationDiagnostics {
        status: "no_change_detected".into(),
        decision_points_available: events.len(),
        perturbation_targets_available: targets_available,
        perturbation_targets_checked: 0,
        decision_points_checked: 0,
        strategy_replays: 1,
        decision_points_sampled: selected_targets.len() < targets_available,
        checks: Vec::with_capacity(selected_targets.len()),
        reason: String::new(),
        interpretation: "For each checked timestamp, one later Bar was perturbed and the strategy replayed from a fresh instance. Decisions and exported diagnostic state strictly before that timestamp were compared with the unmodified replay. This does not inspect strategy-private files, network reads, process-global state, or provider publication history.".into(),
    };

    if selected_targets.is_empty() {
        result.status = "insufficient_work_budget".into();
        result.reason = format!(
            "event count {} exceeds the bounded replay work budget of {} events",
            events.len(),
            MAX_PERTURBATION_REPLAY_EVENTS
        );
        return Ok(result);
    }

    let mut any_change = false;
    for target in selected_targets {
        let mut perturbed = events.to_vec();
        let (price_shift_raw, volume_shift_raw) = perturb_bar(&mut perturbed[target], target)?;
        let trace = replay_decision_trace(context, &perturbed, &mut factory)?;
        result.strategy_replays += 1;
        let mut changed_decision_points = 0;
        let mut changed_decision_fields = BTreeSet::new();
        let mut changed_intermediate_fields = BTreeSet::new();
        for index in 0..target {
            let (baseline_decision, baseline_state) = &baseline[index];
            let (perturbed_decision, perturbed_state) = &trace[index];
            let fields = changed_state_fields(Some(baseline_decision), Some(perturbed_decision));
            let state_fields =
                changed_state_fields(baseline_state.as_ref(), perturbed_state.as_ref());
            if !fields.is_empty() || !state_fields.is_empty() {
                changed_decision_points += 1;
                changed_decision_fields.extend(fields);
                changed_intermediate_fields.extend(state_fields);
            }
        }
        any_change |= changed_decision_points > 0;
        result.perturbation_targets_checked += 1;
        result.decision_points_checked += target;
        result.checks.push(FuturePerturbationCheck {
            perturbed_event_timestamp: events[target].ts(),
            perturbation_kind: "shift_future_bar_ohlcv".into(),
            price_shift_raw,
            volume_shift_raw,
            prior_decision_points_checked: target,
            changed_decision_points,
            changed_decision_fields: changed_decision_fields.into_iter().collect(),
            changed_intermediate_fields: changed_intermediate_fields.into_iter().collect(),
        });
    }
    result.status = if any_change {
        "potential_lookahead_detected".into()
    } else {
        "no_change_detected".into()
    };
    result.reason = if any_change {
        "at least one decision or exported strategy state before a perturbed event changed; inspect strategy determinism and future-data access".into()
    } else {
        "all compared prior decisions and exported state remained stable under the sampled future Bar perturbations".into()
    };
    Ok(result)
}

fn replay_decision_trace<F>(
    context: &StrategyContext,
    events: &[MarketEvent],
    factory: &mut F,
) -> Result<Vec<(Value, Option<Value>)>, String>
where
    F: FnMut() -> Result<Box<dyn Strategy>, String>,
{
    let mut strategy = factory()?;
    let mut init_context = context.clone();
    init_context.as_of = events
        .first()
        .ok_or("future perturbation replay is empty")?
        .ts();
    init_context.validate()?;
    strategy.on_init(&init_context)?;
    events
        .iter()
        .map(|event| {
            let mut event_context = context.clone();
            event_context.as_of = event.ts();
            event_context.validate()?;
            let decision = strategy.on_event(&event_context, event)?;
            Ok((decision_view(&decision)?, strategy.diagnostic_state()))
        })
        .collect()
}

fn perturb_bar(event: &mut MarketEvent, ordinal: usize) -> Result<(i128, i128), String> {
    let MarketEvent::Bar {
        open_raw,
        high_raw,
        low_raw,
        close_raw,
        volume_raw,
        ..
    } = event
    else {
        return Err("future perturbation only supports Bar events".into());
    };
    let delta = (ordinal as i128).saturating_add(1);
    let prices = [*open_raw, *high_raw, *low_raw, *close_raw];
    let shift = if prices
        .iter()
        .all(|price| price.checked_add(delta).is_some())
    {
        delta
    } else if prices
        .iter()
        .all(|price| price.checked_sub(delta).is_some())
    {
        -delta
    } else {
        return Err("未来 Bar 价格位于数值边界，无法安全施加扰动".into());
    };
    *open_raw = open_raw
        .checked_add(shift)
        .ok_or("未来 Bar 开盘价扰动溢出")?;
    *high_raw = high_raw
        .checked_add(shift)
        .ok_or("未来 Bar 最高价扰动溢出")?;
    *low_raw = low_raw
        .checked_add(shift)
        .ok_or("未来 Bar 最低价扰动溢出")?;
    *close_raw = close_raw
        .checked_add(shift)
        .ok_or("未来 Bar 收盘价扰动溢出")?;
    let volume_shift = if let Some(value) = volume_raw.checked_add(delta) {
        *volume_raw = value;
        delta
    } else {
        0
    };
    event.validate()?;
    Ok((shift, volume_shift))
}

/// Compares fresh strategy instances at matching historical event timestamps across startup
/// windows. `max_replays` bounds additional strategy executions; the final timestamp is reused
/// from the terminal comparison. This diagnoses warmup state drift without exposing future events.
pub fn diagnose_startup_trajectory<F>(
    context: &StrategyContext,
    events: &[MarketEvent],
    startup_windows: &[usize],
    max_replays: usize,
    mut factory: F,
) -> Result<StartupTrajectoryDiagnostics, String>
where
    F: FnMut() -> Result<Box<dyn Strategy>, String>,
{
    const MAX_STARTUP_TRAJECTORY_REPLAYS: usize = 1_000;
    if max_replays == 0 || max_replays > MAX_STARTUP_TRAJECTORY_REPLAYS {
        return Err(format!(
            "startup trajectory replay budget must be in 1..={MAX_STARTUP_TRAJECTORY_REPLAYS}"
        ));
    }
    let terminal = diagnose_startup_windows(context, events, startup_windows, &mut factory)?;
    let mut windows = startup_windows.to_vec();
    windows.sort_unstable();
    let reference_window_bars = *windows
        .last()
        .expect("validated startup windows are non-empty");
    let eligible_indices = (0..events.len())
        .filter(|index| index + 1 >= reference_window_bars)
        .collect::<Vec<_>>();
    let mut result = StartupTrajectoryDiagnostics {
        status: "no_change_detected".into(),
        decision_points_available: events.len(),
        decision_points_checked: 0,
        decision_points_sampled: false,
        decision_points_eligible: eligible_indices.len(),
        decision_points_excluded_insufficient_reference_history: events
            .len()
            .saturating_sub(eligible_indices.len()),
        strategy_replays: 0,
        changed_windows_bars: Vec::new(),
        changed_fields_by_window: Default::default(),
        intermediate_state_changed_windows_bars: Vec::new(),
        intermediate_state_changed_fields_by_window: Default::default(),
        divergences: Vec::new(),
        first_divergence: None,
    };
    if windows.len() < 2 {
        result.status = "insufficient_comparison_windows".into();
        return Ok(result);
    }
    if eligible_indices.is_empty() {
        result.status = "insufficient_reference_history".into();
        return Ok(result);
    }
    let points_per_window_set = max_replays / windows.len();
    if points_per_window_set == 0 {
        result.status = "insufficient_check_budget".into();
        return Ok(result);
    }
    let selected_indices = evenly_spaced_indices(eligible_indices.len(), points_per_window_set)
        .into_iter()
        .map(|index| eligible_indices[index])
        .collect::<Vec<_>>();
    let terminal_by_window = terminal
        .windows
        .iter()
        .map(|row| (row.requested_bars, row))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut decision_fields = std::collections::BTreeMap::<usize, BTreeSet<String>>::new();
    let mut state_fields = std::collections::BTreeMap::<usize, BTreeSet<String>>::new();
    for end_index in &selected_indices {
        let end_index = *end_index;
        let end_timestamp = events[end_index].ts();
        let available_events = end_index + 1;
        let mut point_rows = Vec::with_capacity(windows.len());
        for requested in &windows {
            let available = (*requested).min(available_events);
            let start = available_events - available;
            let (decision, state) = if end_index + 1 == events.len() {
                let row = terminal_by_window
                    .get(requested)
                    .expect("terminal comparison has every validated startup window");
                (row.decision.clone(), row.intermediate_state.clone())
            } else {
                let suffix = &events[start..=end_index];
                let mut strategy = factory()?;
                let mut init_context = context.clone();
                init_context.as_of = suffix[0].ts();
                init_context.validate()?;
                strategy.on_init(&init_context)?;
                let mut decision = None;
                for event in suffix {
                    let mut event_context = context.clone();
                    event_context.as_of = event.ts();
                    decision = Some(strategy.on_event(&event_context, event)?);
                }
                result.strategy_replays += 1;
                (
                    decision.ok_or("startup trajectory produced no decision")?,
                    strategy.diagnostic_state(),
                )
            };
            point_rows.push((*requested, decision, state));
        }
        result.decision_points_checked += 1;
        let (_, reference_decision, reference_state) = point_rows
            .last()
            .expect("validated startup windows are non-empty");
        let reference_view = decision_view(reference_decision)?;
        for (requested, decision, state) in point_rows.iter().take(point_rows.len() - 1) {
            let view = decision_view(decision)?;
            let changed = [
                "confidence",
                "priority",
                "signal_id",
                "expires_at",
                "intents",
            ]
            .into_iter()
            .filter(|field| view.get(*field) != reference_view.get(*field))
            .map(str::to_owned)
            .collect::<Vec<_>>();
            let changed_state = changed_state_fields(state.as_ref(), reference_state.as_ref());
            if !changed.is_empty() {
                decision_fields
                    .entry(*requested)
                    .or_default()
                    .extend(changed.iter().cloned());
            }
            if !changed_state.is_empty() {
                state_fields
                    .entry(*requested)
                    .or_default()
                    .extend(changed_state.iter().cloned());
            }
            if !changed.is_empty() || !changed_state.is_empty() {
                let divergence = StartupTrajectoryDivergence {
                    decision_timestamp: end_timestamp,
                    requested_bars: *requested,
                    reference_window_bars: windows[windows.len() - 1],
                    changed_fields: changed,
                    changed_intermediate_fields: changed_state,
                };
                if result.first_divergence.is_none() {
                    result.first_divergence = Some(divergence.clone());
                }
                result.divergences.push(divergence);
            }
        }
    }
    result.decision_points_sampled =
        result.decision_points_checked < result.decision_points_eligible;
    result.changed_windows_bars = decision_fields.keys().copied().collect();
    result.changed_fields_by_window = decision_fields
        .into_iter()
        .map(|(window, fields)| (window, fields.into_iter().collect()))
        .collect();
    result.intermediate_state_changed_windows_bars = state_fields.keys().copied().collect();
    result.intermediate_state_changed_fields_by_window = state_fields
        .into_iter()
        .map(|(window, fields)| (window, fields.into_iter().collect()))
        .collect();
    if result.first_divergence.is_some() {
        result.status = "startup_sensitive".into();
    } else if result.decision_points_eligible == 0 {
        result.status = "insufficient_reference_history".into();
    }
    Ok(result)
}

fn evenly_spaced_indices(total: usize, limit: usize) -> Vec<usize> {
    if total <= limit {
        return (0..total).collect();
    }
    if limit == 1 {
        return vec![total - 1];
    }
    (0..limit)
        .map(|index| index * (total - 1) / (limit - 1))
        .collect()
}

/// Replays suffixes of one event history into fresh strategy instances and compares the
/// decision at the same final event. The factory must create a pristine strategy every time.
/// Strategies may opt in to a deterministic `diagnostic_state` snapshot to expose internal
/// warmup drift. C ABI adapters without a diagnostic callback continue to report no state.
pub fn diagnose_startup_windows<F>(
    context: &StrategyContext,
    events: &[MarketEvent],
    startup_windows: &[usize],
    mut factory: F,
) -> Result<StartupDiagnostics, String>
where
    F: FnMut() -> Result<Box<dyn Strategy>, String>,
{
    if events.is_empty() {
        return Err("startup diagnostics require at least one event".into());
    }
    if startup_windows.is_empty() || startup_windows.contains(&0) {
        return Err("startup windows must be non-empty positive integers".into());
    }
    let unique = startup_windows.iter().copied().collect::<BTreeSet<_>>();
    if unique.len() != startup_windows.len() {
        return Err("startup windows must be unique".into());
    }
    for event in events {
        event.validate()?;
    }
    if events.windows(2).any(|pair| pair[0].ts() >= pair[1].ts()) {
        return Err("startup diagnostic events must be strictly increasing by timestamp".into());
    }
    let mut windows = startup_windows.to_vec();
    windows.sort_unstable();
    let final_event = events.last().expect("non-empty checked");
    let mut observations = Vec::with_capacity(windows.len());
    for requested in &windows {
        let available = (*requested).min(events.len());
        let suffix = &events[events.len() - available..];
        let mut strategy = factory()?;
        let mut init_context = context.clone();
        init_context.as_of = suffix[0].ts();
        init_context.validate()?;
        strategy.on_init(&init_context)?;
        let mut decision: Option<StrategyDecision> = None;
        for event in suffix {
            let mut event_context = context.clone();
            event_context.as_of = event.ts();
            decision = Some(strategy.on_event(&event_context, event)?);
        }
        let decision = decision.ok_or("startup diagnostic produced no decision")?;
        observations.push(StartupWindowObservation {
            requested_bars: *requested,
            available_bars: available,
            history_truncated: available < events.len(),
            decision,
            changed_fields: Vec::new(),
            intermediate_state: strategy.diagnostic_state(),
            changed_intermediate_fields: Vec::new(),
        });
    }
    let reference = observations
        .last()
        .expect("non-empty startup windows checked")
        .decision
        .clone();
    let reference_view = decision_view(&reference)?;
    let reference_state = observations
        .last()
        .expect("non-empty startup windows checked")
        .intermediate_state
        .clone();
    for observation in &mut observations {
        let view = decision_view(&observation.decision)?;
        observation.changed_fields = [
            "confidence",
            "priority",
            "signal_id",
            "expires_at",
            "intents",
        ]
        .into_iter()
        .filter(|field| view.get(*field) != reference_view.get(*field))
        .map(str::to_owned)
        .collect();
        observation.changed_intermediate_fields = changed_state_fields(
            observation.intermediate_state.as_ref(),
            reference_state.as_ref(),
        );
    }
    let sensitive = observations
        .iter()
        .any(|row| !row.changed_fields.is_empty() || !row.changed_intermediate_fields.is_empty());
    let reference = observations
        .last()
        .expect("non-empty startup windows checked");
    let reference_satisfied = reference.available_bars >= reference.requested_bars;
    Ok(StartupDiagnostics {
        status: if sensitive {
            "startup_sensitive".into()
        } else if !reference_satisfied {
            "insufficient_reference_history".into()
        } else {
            "stable_across_requested_windows".into()
        },
        reference_window_bars: *windows.last().expect("non-empty startup windows checked"),
        decision_timestamp: final_event.ts(),
        windows: observations,
        interpretation: "Different terminal decisions or exported intermediate state across requested history windows identify startup sensitivity only for this event sample and these windows; the largest requested window is the reference. Strategies that do not export state are compared by terminal decision only.".into(),
    })
}

fn changed_state_fields(observed: Option<&Value>, reference: Option<&Value>) -> Vec<String> {
    match (observed, reference) {
        (None, None) => Vec::new(),
        (Some(_), None) | (None, Some(_)) => vec!["__availability__".into()],
        (Some(Value::Object(observed)), Some(Value::Object(reference))) => observed
            .keys()
            .chain(reference.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|key| observed.get(*key) != reference.get(*key))
            .map(|key| key.to_string())
            .collect(),
        (Some(observed), Some(reference)) if observed != reference => vec!["__value__".into()],
        (Some(_), Some(_)) => Vec::new(),
    }
}

fn decision_view(decision: &StrategyDecision) -> Result<Value, String> {
    serde_json::to_value(serde_json::json!({
        "confidence": decision.confidence,
        "priority": decision.priority,
        "signal_id": decision.signal_id,
        "expires_at": decision.expires_at,
        "intents": decision.intents,
    }))
    .map_err(|error| format!("encode startup diagnostic decision: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StrategyOrderIntent, STRATEGY_API_VERSION};
    use qx_core::InstrumentId;

    struct CountingStrategy(usize);
    impl Strategy for CountingStrategy {
        fn diagnostic_state(&self) -> Option<Value> {
            Some(serde_json::json!({"events_seen": self.0}))
        }

        fn on_event(
            &mut self,
            context: &StrategyContext,
            _event: &MarketEvent,
        ) -> Result<StrategyDecision, String> {
            context.validate()?;
            self.0 += 1;
            Ok(StrategyDecision {
                schema_version: STRATEGY_API_VERSION,
                request_id: "diagnostic".into(),
                strategy_id: context.strategy_id.clone(),
                signal_id: self.0 as u64,
                confidence: self.0 as i128,
                priority: 0,
                expires_at: context.as_of,
                intents: Vec::<StrategyOrderIntent>::new(),
            })
        }
    }

    fn context() -> StrategyContext {
        StrategyContext {
            strategy_id: "startup-test".into(),
            strategy_version: "1".into(),
            account_id: "account".into(),
            venue_id: "venue".into(),
            data_fingerprint: "fingerprint".into(),
            as_of: 1,
            positions_exact: std::collections::BTreeMap::new(),
            cash_exact: std::collections::BTreeMap::new(),
            available_margin_exact: None,
            positions: Default::default(),
            cash: Default::default(),
            available_margin_raw: None,
            risk_state: "verified".into(),
        }
    }

    fn events() -> Vec<MarketEvent> {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        (1..=5)
            .map(|ts| MarketEvent::Bar {
                instrument: instrument.clone(),
                ts,
                open_raw: 100,
                high_raw: 101,
                low_raw: 99,
                close_raw: 100,
                volume_raw: 10,
                exact_ohlcv: None,
            })
            .collect()
    }

    #[test]
    fn future_perturbation_checks_prior_decisions_with_a_bounded_replay_budget() {
        let result = diagnose_future_perturbations(&context(), &events(), 2, || {
            Ok(Box::new(CountingStrategy(0)))
        })
        .unwrap();

        assert_eq!(result.status, "no_change_detected");
        assert_eq!(result.decision_points_available, 5);
        assert_eq!(result.perturbation_targets_available, 4);
        assert_eq!(result.perturbation_targets_checked, 2);
        assert_eq!(result.decision_points_checked, 5);
        assert_eq!(result.strategy_replays, 3);
        assert!(result.decision_points_sampled);
        assert_eq!(result.checks[0].perturbation_kind, "shift_future_bar_ohlcv");
        assert!(result.checks[0].price_shift_raw > 0);
        assert!(result.checks[0].volume_shift_raw > 0);
        assert_eq!(
            result
                .checks
                .iter()
                .map(|check| check.prior_decision_points_checked)
                .collect::<Vec<_>>(),
            [1, 4]
        );
        assert!(result
            .checks
            .iter()
            .all(|check| check.changed_decision_points == 0));
    }

    #[test]
    fn future_perturbation_flags_replay_drift_before_the_changed_event() {
        let mut created = 0;
        let result = diagnose_future_perturbations(&context(), &events(), 1, || {
            created += 1;
            Ok(Box::new(CountingStrategy(created * 100)))
        })
        .unwrap();

        assert_eq!(result.status, "potential_lookahead_detected");
        assert_eq!(result.perturbation_targets_checked, 1);
        assert!(result.checks[0].changed_decision_points > 0);
        assert!(result.reason.contains("inspect strategy determinism"));
    }

    #[test]
    fn future_perturbation_reports_insufficient_history_and_rejects_unbounded_budgets() {
        let single_event = events().into_iter().take(1).collect::<Vec<_>>();
        let result = diagnose_future_perturbations(&context(), &single_event, 1, || {
            Ok(Box::new(CountingStrategy(0)))
        })
        .unwrap();
        assert_eq!(result.status, "insufficient_history");
        assert_eq!(result.perturbation_targets_checked, 0);
        assert!(
            diagnose_future_perturbations(&context(), &events(), 65, || {
                Ok(Box::new(CountingStrategy(0)))
            })
            .is_err()
        );
    }

    #[test]
    fn future_perturbation_fails_closed_above_the_memory_bounded_trace_limit() {
        let mut template = events()[0].clone();
        let large = (0..65_537)
            .map(|index| {
                if let MarketEvent::Bar { ts, .. } = &mut template {
                    *ts = index as u64 + 1;
                }
                template.clone()
            })
            .collect::<Vec<_>>();
        let result = diagnose_future_perturbations(&context(), &large, 16, || {
            Ok(Box::new(CountingStrategy(0)))
        })
        .unwrap();

        assert_eq!(result.status, "insufficient_work_budget");
        assert_eq!(result.strategy_replays, 0);
        assert_eq!(result.perturbation_targets_checked, 0);
        assert!(result
            .reason
            .contains("memory-bounded diagnostic trace limit"));
    }

    #[test]
    fn fresh_native_strategies_detect_startup_sensitive_window() {
        let result = diagnose_startup_windows(&context(), &events(), &[1, 3, 5], || {
            Ok(Box::new(CountingStrategy(0)))
        })
        .unwrap();
        assert_eq!(result.status, "startup_sensitive");
        assert_eq!(result.reference_window_bars, 5);
        assert_eq!(result.decision_timestamp, 5);
        assert_eq!(
            result.windows[0].changed_fields,
            ["confidence", "signal_id"]
        );
        assert_eq!(
            result.windows[0].changed_intermediate_fields,
            ["events_seen"]
        );
        assert_eq!(result.windows[0].available_bars, 1);
        assert!(result.windows[0].history_truncated);
    }

    #[test]
    fn trajectory_compares_matching_timestamps_with_a_bounded_replay_budget() {
        let result = diagnose_startup_trajectory(&context(), &events(), &[2, 4], 10, || {
            Ok(Box::new(CountingStrategy(0)))
        })
        .unwrap();

        assert_eq!(result.status, "startup_sensitive");
        assert_eq!(result.decision_points_available, 5);
        assert_eq!(result.decision_points_eligible, 2);
        assert_eq!(
            result.decision_points_excluded_insufficient_reference_history,
            3
        );
        assert_eq!(result.decision_points_checked, 2);
        assert!(!result.decision_points_sampled);
        assert_eq!(result.strategy_replays, 2);
        assert_eq!(result.changed_windows_bars, [2]);
        assert_eq!(
            result.first_divergence.as_ref().unwrap().decision_timestamp,
            4
        );
        assert_eq!(
            result.first_divergence.as_ref().unwrap().changed_fields,
            ["confidence", "signal_id"]
        );
        assert_eq!(
            result
                .divergences
                .iter()
                .map(|row| row.decision_timestamp)
                .collect::<Vec<_>>(),
            [4, 5]
        );
        assert_eq!(
            result.divergences[0],
            result.first_divergence.clone().unwrap()
        );
    }

    #[test]
    fn insufficient_reference_history_is_not_counted_as_a_warmup_comparison() {
        let result = diagnose_startup_trajectory(&context(), &events(), &[2, 6], 10, || {
            Ok(Box::new(CountingStrategy(0)))
        })
        .unwrap();

        assert_eq!(result.status, "insufficient_reference_history");
        assert_eq!(result.decision_points_available, 5);
        assert_eq!(result.decision_points_eligible, 0);
        assert_eq!(
            result.decision_points_excluded_insufficient_reference_history,
            5
        );
        assert_eq!(result.decision_points_checked, 0);
        assert!(result.divergences.is_empty());

        let terminal = diagnose_startup_windows(&context(), &events(), &[2, 6], || {
            Ok(Box::new(CountingStrategy(0)))
        })
        .unwrap();
        assert_eq!(terminal.status, "startup_sensitive");
    }

    #[test]
    fn legacy_trajectory_json_without_divergence_list_remains_readable() {
        let legacy = serde_json::json!({
            "status": "startup_sensitive",
            "decision_points_available": 5,
            "decision_points_checked": 1,
            "decision_points_sampled": true,
            "strategy_replays": 1,
            "changed_windows_bars": [2],
            "changed_fields_by_window": {"2": ["signal_id"]},
            "intermediate_state_changed_windows_bars": [],
            "intermediate_state_changed_fields_by_window": {},
            "first_divergence": {
                "decision_timestamp": 5,
                "requested_bars": 2,
                "reference_window_bars": 4,
                "changed_fields": ["signal_id"],
                "changed_intermediate_fields": []
            }
        });
        let parsed: StartupTrajectoryDiagnostics = serde_json::from_value(legacy).unwrap();
        assert!(parsed.divergences.is_empty());
        assert_eq!(
            parsed.first_divergence.as_ref().unwrap().decision_timestamp,
            5
        );
        let normalized = serde_json::to_value(parsed).unwrap();
        assert_eq!(normalized["changed_windows_events"], serde_json::json!([2]));
        assert_eq!(
            normalized["first_divergence"]["requested_events"],
            serde_json::json!(2)
        );
        assert!(normalized["changed_windows_bars"].is_null());
    }

    #[test]
    fn startup_diagnostic_wire_names_describe_generic_event_windows() {
        let result = diagnose_startup_windows(&context(), &events(), &[2, 5], || {
            Ok(Box::new(CountingStrategy(0)))
        })
        .unwrap();
        let encoded = serde_json::to_value(result).unwrap();
        assert_eq!(encoded["reference_window_events"], serde_json::json!(5));
        assert_eq!(
            encoded["windows"][0]["requested_events"],
            serde_json::json!(2)
        );
        assert_eq!(
            encoded["windows"][0]["available_events"],
            serde_json::json!(2)
        );
        assert!(encoded["windows"][0]["requested_bars"].is_null());
    }

    #[test]
    fn rejects_duplicate_or_out_of_order_diagnostic_inputs() {
        assert!(
            diagnose_startup_windows(&context(), &events(), &[2, 2], || {
                Ok(Box::new(CountingStrategy(0)))
            })
            .is_err()
        );
        let mut bad_events = events();
        bad_events.swap(1, 2);
        assert!(diagnose_startup_windows(&context(), &bad_events, &[2], || {
            Ok(Box::new(CountingStrategy(0)))
        })
        .is_err());
    }
}
