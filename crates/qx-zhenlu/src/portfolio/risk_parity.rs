//! Deterministic long-only equal-risk-contribution portfolio construction.
//!
//! This is a planning kernel only: it returns target weights and order deltas; it
//! never submits orders. Return observations and mark prices are frozen inputs,
//! so callers can persist their hashes alongside the resulting plan.

use qx_core::{InstrumentId, SCALE};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const BPS: i128 = 10_000;
const RISK_WEIGHT_SCALE: i128 = 1_000_000;
const RISK_BUDGET: i128 = RISK_WEIGHT_SCALE * RISK_WEIGHT_SCALE;
const MAX_ASSETS: usize = 64;
const MAX_RETURN_OBSERVATIONS: usize = 10_000;
const MAX_ABS_RETURN_BPS: i64 = 1_000;
const MAX_COORDINATE_SWEEPS: usize = 10_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RiskParityAsset {
    pub instrument: InstrumentId,
    /// Positive mark price in qx fixed-point units.
    pub mark_price_raw: i128,
    /// Aligned close-to-close returns in basis points, oldest first.
    pub returns_bps: Vec<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RiskParityOrderDelta {
    pub instrument: InstrumentId,
    pub delta_qty_raw: i128,
    pub estimated_notional_raw: i128,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RiskParityAllocation {
    pub instrument: InstrumentId,
    pub weight_bps: u32,
    pub target_notional_raw: i128,
    pub target_qty_raw: i128,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RiskParityPlan {
    pub allocations: Vec<RiskParityAllocation>,
    pub orders: Vec<RiskParityOrderDelta>,
    pub target_gross_notional_raw: i128,
    pub current_gross_notional_raw: i128,
    pub turnover_bps: u32,
    pub observations: usize,
    pub solver_sweeps: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RiskParityInput {
    pub available_cash_raw: i128,
    pub target_gross_notional_raw: i128,
    pub current_positions: BTreeMap<String, i128>,
    pub assets: Vec<RiskParityAsset>,
    pub max_turnover_bps: u32,
    pub min_trade_notional_raw: i128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RiskParityError {
    InvalidInput(String),
    InsufficientHistory(String),
    Arithmetic(String),
    SolverDidNotConverge,
}

impl fmt::Display for RiskParityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message)
            | Self::InsufficientHistory(message)
            | Self::Arithmetic(message) => formatter.write_str(message),
            Self::SolverDidNotConverge => write!(
                formatter,
                "risk parity solver did not converge within {MAX_COORDINATE_SWEEPS} bounded sweeps"
            ),
        }
    }
}

impl From<&str> for RiskParityError {
    fn from(message: &str) -> Self {
        Self::Arithmetic(message.into())
    }
}

impl From<String> for RiskParityError {
    fn from(message: String) -> Self {
        Self::InvalidInput(message)
    }
}

/// Build an equal-risk-contribution target using the full sample covariance
/// matrix, then convert target notionals into deterministic signed quantity deltas.
/// Long-only weights sum to exactly 10,000 bps; no leverage is inferred.
pub fn plan_risk_parity(input: &RiskParityInput) -> Result<RiskParityPlan, RiskParityError> {
    validate_input(input)?;
    let mut assets = input.assets.clone();
    assets.sort_by_key(|asset| asset.instrument.to_string());

    let covariance = covariance_matrix(&assets)?;
    let (risk_weights, solver_sweeps) = equal_risk_weights(&covariance)?;
    let weights_bps = normalize_weights_bps(&risk_weights)?;
    let observations = assets[0].returns_bps.len();

    let mut allocations = Vec::with_capacity(assets.len());
    let mut orders = Vec::with_capacity(assets.len() + input.current_positions.len());
    let mut allocated_notional = 0_i128;
    let mut current_gross = 0_i128;
    let mut turnover_notional = 0_i128;
    let asset_keys = assets
        .iter()
        .map(|asset| asset.instrument.to_string())
        .collect::<BTreeSet<_>>();

    for (index, asset) in assets.iter().enumerate() {
        let key = asset.instrument.to_string();
        let target_notional = if index + 1 == assets.len() {
            input
                .target_gross_notional_raw
                .checked_sub(allocated_notional)
                .ok_or("risk parity target allocation overflows")?
        } else {
            input
                .target_gross_notional_raw
                .checked_mul(i128::from(weights_bps[index]))
                .and_then(|value| value.checked_div(BPS))
                .ok_or("risk parity target allocation overflows")?
        };
        allocated_notional = allocated_notional
            .checked_add(target_notional)
            .ok_or("risk parity target allocation total overflows")?;
        let target_qty = target_notional
            .checked_mul(SCALE)
            .and_then(|value| value.checked_div(asset.mark_price_raw))
            .ok_or("risk parity target quantity overflows")?;
        let current_qty = input
            .current_positions
            .get(&key)
            .copied()
            .unwrap_or_default();
        let current_notional = current_qty
            .unsigned_abs()
            .try_into()
            .ok()
            .and_then(|qty: i128| qty.checked_mul(asset.mark_price_raw))
            .and_then(|value| value.checked_div(SCALE))
            .ok_or("risk parity current notional overflows")?;
        current_gross = current_gross
            .checked_add(current_notional)
            .ok_or("risk parity current gross notional overflows")?;
        let delta = target_qty
            .checked_sub(current_qty)
            .ok_or("risk parity order delta overflows")?;
        let delta_notional = delta
            .checked_abs()
            .and_then(|qty| qty.checked_mul(asset.mark_price_raw))
            .and_then(|value| value.checked_div(SCALE))
            .ok_or("risk parity order notional overflows")?;
        turnover_notional = turnover_notional
            .checked_add(delta_notional)
            .ok_or("risk parity turnover notional overflows")?;
        if delta != 0 && delta_notional >= input.min_trade_notional_raw {
            orders.push(RiskParityOrderDelta {
                instrument: asset.instrument.clone(),
                delta_qty_raw: delta,
                estimated_notional_raw: delta_notional,
            });
        }
        allocations.push(RiskParityAllocation {
            instrument: asset.instrument.clone(),
            weight_bps: weights_bps[index],
            target_notional_raw: target_notional,
            target_qty_raw: target_qty,
        });
    }

    // Held assets outside the risk universe are not silently forgotten: they
    // must be explicitly marked and are planned for closure at their supplied mark.
    for (key, current_qty) in &input.current_positions {
        if asset_keys.contains(key) || *current_qty == 0 {
            continue;
        }
        return Err(format!(
            "held instrument {key} is missing a risk-parity mark and return series"
        )
        .into());
    }

    let denominator = current_gross.max(input.target_gross_notional_raw).max(1);
    let turnover_bps = turnover_notional
        .checked_mul(BPS)
        .and_then(|value| value.checked_div(denominator))
        .ok_or("risk parity turnover calculation overflows")?;
    let turnover_bps = u32::try_from(turnover_bps).unwrap_or(u32::MAX);
    if turnover_bps > input.max_turnover_bps {
        return Err(format!(
            "risk parity turnover {turnover_bps} bps exceeds limit {} bps",
            input.max_turnover_bps
        )
        .into());
    }
    Ok(RiskParityPlan {
        allocations,
        orders,
        target_gross_notional_raw: input.target_gross_notional_raw,
        current_gross_notional_raw: current_gross,
        turnover_bps,
        observations,
        solver_sweeps,
    })
}

fn validate_input(input: &RiskParityInput) -> Result<(), RiskParityError> {
    if input.available_cash_raw <= 0 || input.target_gross_notional_raw <= 0 {
        return Err("risk parity cash and target gross notional must be positive".into());
    }
    if input.target_gross_notional_raw > input.available_cash_raw {
        return Err("risk parity target gross notional exceeds available cash".into());
    }
    if input.assets.is_empty() || input.assets.len() > MAX_ASSETS {
        return Err(format!("risk parity asset count must be in 1..={MAX_ASSETS}").into());
    }
    if input.max_turnover_bps > 10_000 {
        return Err("risk parity max turnover cannot exceed 10000 bps".into());
    }
    if input.min_trade_notional_raw < 0 {
        return Err("risk parity minimum trade notional cannot be negative".into());
    }
    let observations = input.assets[0].returns_bps.len();
    if !(2..=MAX_RETURN_OBSERVATIONS).contains(&observations) {
        return Err(format!(
            "risk parity return observations must be in 2..={MAX_RETURN_OBSERVATIONS}"
        )
        .into());
    }
    let mut instruments = BTreeSet::new();
    for asset in &input.assets {
        if asset.mark_price_raw <= 0 {
            return Err(format!("risk parity mark must be positive: {}", asset.instrument).into());
        }
        let key = asset.instrument.to_string();
        if !instruments.insert(key.clone()) {
            return Err(format!("duplicate risk parity instrument: {key}").into());
        }
        if asset.returns_bps.len() != observations {
            return Err("risk parity return series must have the same aligned length".into());
        }
        if asset
            .returns_bps
            .iter()
            .any(|value| value.unsigned_abs() > MAX_ABS_RETURN_BPS as u64)
        {
            return Err(format!("risk parity return is out of range: {key}").into());
        }
    }
    for key in input.current_positions.keys() {
        InstrumentId::parse(key).ok_or_else(|| format!("invalid held instrument id: {key}"))?;
    }
    Ok(())
}

fn covariance_matrix(assets: &[RiskParityAsset]) -> Result<Vec<Vec<i128>>, RiskParityError> {
    let observations = i128::try_from(assets[0].returns_bps.len())
        .map_err(|_| "risk parity observation count overflows")?;
    let sums = assets
        .iter()
        .map(|asset| {
            asset
                .returns_bps
                .iter()
                .try_fold(0_i128, |sum, value| sum.checked_add(i128::from(*value)))
                .ok_or("risk parity return sum overflows")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut covariance = vec![vec![0_i128; assets.len()]; assets.len()];
    for row in 0..assets.len() {
        for column in row..assets.len() {
            let products = assets[row]
                .returns_bps
                .iter()
                .zip(&assets[column].returns_bps)
                .try_fold(0_i128, |sum, (left, right)| {
                    i128::from(*left)
                        .checked_mul(i128::from(*right))
                        .and_then(|value| sum.checked_add(value))
                })
                .ok_or("risk parity covariance overflows")?;
            // Keep the exact centered covariance numerator `n * sum(x*y) - sum(x)*sum(y)`.
            // The omitted positive denominator is common to every cell, so ERC weights are
            // unchanged. Avoiding integer mean division preserves low-volatility samples.
            let value = products
                .checked_mul(observations)
                .and_then(|value| value.checked_sub(sums[row].checked_mul(sums[column])?))
                .ok_or("risk parity centered covariance overflows")?;
            covariance[row][column] = value;
            covariance[column][row] = value;
        }
    }
    if (0..assets.len()).any(|index| covariance[index][index] <= 0) {
        return Err(RiskParityError::InsufficientHistory(
            "risk parity requires positive variance for every asset".into(),
        ));
    }
    Ok(covariance)
}

fn equal_risk_weights(covariance: &[Vec<i128>]) -> Result<(Vec<i128>, usize), RiskParityError> {
    let count = covariance.len();
    let mut weights = vec![RISK_WEIGHT_SCALE; count];
    for sweep in 1..=MAX_COORDINATE_SWEEPS {
        let mut largest_change = 0_i128;
        for index in 0..count {
            let diagonal = covariance[index][index];
            let cross = (0..count)
                .filter(|other| *other != index)
                .try_fold(0_i128, |sum, other| {
                    covariance[index][other]
                        .checked_mul(weights[other])
                        .and_then(|term| sum.checked_add(term))
                })
                .ok_or("risk parity coordinate sum overflows")?;
            let discriminant = cross
                .checked_mul(cross)
                .and_then(|value| {
                    diagonal
                        .checked_mul(RISK_BUDGET)
                        .and_then(|scaled| scaled.checked_mul(4))
                        .and_then(|term| value.checked_add(term))
                })
                .filter(|value| *value >= 0)
                .ok_or("risk parity covariance is not numerically solvable")?;
            let root = integer_sqrt(discriminant);
            let updated = if cross >= 0 {
                let denominator = root
                    .checked_add(cross)
                    .ok_or("risk parity solver denominator overflows")?;
                if denominator <= 0 {
                    return Err("risk parity solver denominator is not positive".into());
                }
                RISK_BUDGET
                    .checked_mul(2)
                    .and_then(|value| value.checked_div(denominator))
                    .ok_or("risk parity solver update overflows")?
            } else {
                root.checked_sub(cross)
                    .and_then(|value| diagonal.checked_mul(2).and_then(|d| value.checked_div(d)))
                    .ok_or("risk parity solver update overflows")?
            };
            if updated <= 0 {
                return Err("risk parity solver produced a non-positive weight".into());
            }
            largest_change = largest_change.max(
                updated
                    .checked_sub(weights[index])
                    .and_then(i128::checked_abs)
                    .ok_or("risk parity solver delta overflows")?,
            );
            weights[index] = updated;
        }
        if largest_change <= 1 {
            return Ok((weights, sweep));
        }
    }
    Err(RiskParityError::SolverDidNotConverge)
}

fn normalize_weights_bps(weights: &[i128]) -> Result<Vec<u32>, RiskParityError> {
    let total = weights
        .iter()
        .try_fold(0_i128, |sum, value| sum.checked_add(*value))
        .ok_or("risk parity weight total overflows")?;
    if total <= 0 {
        return Err("risk parity weight total is not positive".into());
    }
    let mut normalized = Vec::with_capacity(weights.len());
    let mut assigned = 0_u32;
    for (index, weight) in weights.iter().enumerate() {
        let value = if index + 1 == weights.len() {
            10_000_u32.saturating_sub(assigned)
        } else {
            let value = weight
                .checked_mul(BPS)
                .and_then(|number| number.checked_div(total))
                .ok_or("risk parity normalized weight overflows")?;
            let value = u32::try_from(value).map_err(|_| "risk parity weight is out of range")?;
            assigned = assigned
                .checked_add(value)
                .ok_or("risk parity normalized weight sum overflows")?;
            value
        };
        normalized.push(value);
    }
    Ok(normalized)
}

fn integer_sqrt(value: i128) -> i128 {
    if value <= 0 {
        return 0;
    }
    let mut low = 1_i128;
    let mut high = value.min(1_i128 << 64);
    while low <= high {
        let middle = low + (high - low) / 2;
        let quotient = value / middle;
        if middle == quotient {
            return middle;
        }
        if middle > quotient {
            high = middle - 1;
        } else {
            low = middle + 1;
        }
    }
    high
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instrument(symbol: &str) -> InstrumentId {
        InstrumentId::parse(&format!("{symbol}.SIM")).unwrap()
    }

    fn input() -> RiskParityInput {
        RiskParityInput {
            available_cash_raw: 20_000 * SCALE,
            target_gross_notional_raw: 10_000 * SCALE,
            current_positions: BTreeMap::new(),
            assets: vec![
                RiskParityAsset {
                    instrument: instrument("BTC"),
                    mark_price_raw: 100 * SCALE,
                    returns_bps: vec![100, -100, 100, -100],
                },
                RiskParityAsset {
                    instrument: instrument("ETH"),
                    mark_price_raw: 100 * SCALE,
                    returns_bps: vec![200, -200, -200, 200],
                },
            ],
            max_turnover_bps: 10_000,
            min_trade_notional_raw: 0,
        }
    }

    #[test]
    fn uncorrelated_assets_receive_equal_risk_contributions_and_exact_budget() {
        let plan = plan_risk_parity(&input()).unwrap();
        assert_eq!(plan.allocations[0].weight_bps, 6_666);
        assert_eq!(plan.allocations[1].weight_bps, 3_334);
        assert_eq!(
            plan.allocations
                .iter()
                .map(|allocation| allocation.target_notional_raw)
                .sum::<i128>(),
            10_000 * SCALE
        );
        assert_eq!(plan.observations, 4);
        assert!(plan.solver_sweeps <= MAX_COORDINATE_SWEEPS);
    }

    #[test]
    fn low_volatility_returns_keep_exact_covariance_and_scale_inverse_volatility_weights() {
        let mut request = input();
        request.assets[0].returns_bps = vec![1, 0, 1, 0];
        request.assets[1].returns_bps = vec![2, 0, 2, 0];
        let plan = plan_risk_parity(&request).expect("integer return changes remain measurable");
        let weights = plan
            .allocations
            .iter()
            .map(|allocation| allocation.weight_bps)
            .collect::<Vec<_>>();
        assert_eq!(weights, vec![6_666, 3_334]);
    }

    #[test]
    fn current_holdings_become_signed_order_deltas_and_turnover_is_notional_based() {
        let mut request = input();
        request
            .current_positions
            .insert("BTC.SIM".into(), 50 * SCALE);
        let plan = plan_risk_parity(&request).unwrap();
        assert_eq!(plan.orders.len(), 2);
        let btc = plan
            .orders
            .iter()
            .find(|order| order.instrument == instrument("BTC"))
            .unwrap();
        assert!(btc.delta_qty_raw > 0);
        assert_eq!(plan.current_gross_notional_raw, 5_000 * SCALE);
        assert!(plan.turnover_bps <= 10_000);
    }

    #[test]
    fn malformed_or_unaligned_risk_inputs_fail_closed() {
        let mut request = input();
        request.assets[1].returns_bps.pop();
        assert!(plan_risk_parity(&request)
            .unwrap_err()
            .to_string()
            .contains("same aligned length"));

        let mut request = input();
        request.assets[0].returns_bps = vec![10; 4];
        assert!(plan_risk_parity(&request)
            .unwrap_err()
            .to_string()
            .contains("positive variance"));
    }

    #[test]
    fn cash_budget_turnover_and_held_universe_are_enforced() {
        let mut request = input();
        request.target_gross_notional_raw = request.available_cash_raw + 1;
        assert!(plan_risk_parity(&request)
            .unwrap_err()
            .to_string()
            .contains("exceeds available cash"));

        let mut request = input();
        request.max_turnover_bps = 1;
        assert!(plan_risk_parity(&request)
            .unwrap_err()
            .to_string()
            .contains("exceeds limit"));

        let mut request = input();
        request.current_positions.insert("SOL.SIM".into(), SCALE);
        assert!(plan_risk_parity(&request)
            .unwrap_err()
            .to_string()
            .contains("missing a risk-parity mark"));
    }
}
