//! Deterministic parent-order slicing primitives shared by execution clients.
//!
//! This module only plans child quantities and decision timestamps. It never
//! submits orders: every child still has to pass through the normal risk and
//! execution gateway.

use serde::{Deserialize, Serialize};
use std::fmt;

pub const MAX_ALGO_CHILDREN: usize = 10_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlgoChildSlice {
    pub parent_id: String,
    pub child_index: u32,
    pub scheduled_ts: u64,
    pub quantity_raw: i128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VwapVolumeBucket {
    pub ts: u64,
    pub market_volume_raw: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AlgoExecutionError {
    InvalidParent,
    InvalidRange,
    InvalidQuantity,
    InvalidChildCount,
    InvalidVolumeProfile,
    ArithmeticOverflow,
}

impl fmt::Display for AlgoExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidParent => "algo parent id must not be empty",
            Self::InvalidRange => "algo schedule end must be greater than start",
            Self::InvalidQuantity => "parent quantity must be positive",
            Self::InvalidChildCount => "child count must be in 1..=10000 and not exceed quantity",
            Self::InvalidVolumeProfile => {
                "VWAP volume buckets must be ordered, positive and non-empty"
            }
            Self::ArithmeticOverflow => "algo schedule arithmetic overflow",
        })
    }
}

impl std::error::Error for AlgoExecutionError {}

/// Split a parent quantity into near-equal slices distributed over the closed
/// `[start_ts, end_ts]` interval. Remainder units go to earlier slices so the
/// result is deterministic and exactly conserves the parent quantity.
pub fn plan_twap(
    parent_id: &str,
    total_quantity_raw: i128,
    start_ts: u64,
    end_ts: u64,
    child_count: usize,
) -> Result<Vec<AlgoChildSlice>, AlgoExecutionError> {
    validate_common(parent_id, total_quantity_raw, start_ts, end_ts, child_count)?;
    let count = child_count as i128;
    let base = total_quantity_raw / count;
    let remainder = total_quantity_raw % count;
    let span = u128::from(end_ts - start_ts);
    let denominator = child_count.saturating_sub(1).max(1) as u128;
    (0..child_count)
        .map(|index| {
            let offset = if child_count == 1 {
                0
            } else {
                span.checked_mul(index as u128)
                    .ok_or(AlgoExecutionError::ArithmeticOverflow)?
                    / denominator
            };
            let scheduled_ts = u128::from(start_ts)
                .checked_add(offset)
                .and_then(|value| u64::try_from(value).ok())
                .ok_or(AlgoExecutionError::ArithmeticOverflow)?;
            Ok(AlgoChildSlice {
                parent_id: parent_id.to_owned(),
                child_index: index as u32,
                scheduled_ts,
                quantity_raw: base + if (index as i128) < remainder { 1 } else { 0 },
            })
        })
        .collect()
}

/// Allocate parent quantity proportional to a frozen market-volume profile.
/// The last bucket receives the integer rounding remainder, preserving the
/// exact parent quantity. The profile is bounded and must contain strictly
/// increasing timestamps and positive volumes.
pub fn plan_vwap(
    parent_id: &str,
    total_quantity_raw: i128,
    start_ts: u64,
    end_ts: u64,
    buckets: &[VwapVolumeBucket],
) -> Result<Vec<AlgoChildSlice>, AlgoExecutionError> {
    let count = buckets.len();
    validate_common(parent_id, total_quantity_raw, start_ts, end_ts, count)?;
    if buckets.iter().any(|bucket| bucket.market_volume_raw == 0)
        || buckets
            .iter()
            .any(|bucket| bucket.ts < start_ts || bucket.ts > end_ts)
        || buckets.windows(2).any(|pair| pair[0].ts >= pair[1].ts)
    {
        return Err(AlgoExecutionError::InvalidVolumeProfile);
    }
    let total_volume = buckets.iter().try_fold(0_u128, |sum, bucket| {
        sum.checked_add(bucket.market_volume_raw)
            .ok_or(AlgoExecutionError::ArithmeticOverflow)
    })?;
    if total_volume == 0 {
        return Err(AlgoExecutionError::InvalidVolumeProfile);
    }
    let parent_qty = total_quantity_raw as u128;
    let mut assigned = 0_u128;
    let mut slices = Vec::with_capacity(count);
    for (index, bucket) in buckets.iter().enumerate() {
        let quantity = if index + 1 == count {
            parent_qty
                .checked_sub(assigned)
                .ok_or(AlgoExecutionError::ArithmeticOverflow)?
        } else {
            mul_div_floor(parent_qty, bucket.market_volume_raw, total_volume)?
        };
        assigned = assigned
            .checked_add(quantity)
            .ok_or(AlgoExecutionError::ArithmeticOverflow)?;
        if quantity > 0 {
            slices.push(AlgoChildSlice {
                parent_id: parent_id.to_owned(),
                child_index: index as u32,
                scheduled_ts: bucket.ts,
                quantity_raw: i128::try_from(quantity)
                    .map_err(|_| AlgoExecutionError::ArithmeticOverflow)?,
            });
        }
    }
    if assigned != parent_qty {
        return Err(AlgoExecutionError::ArithmeticOverflow);
    }
    Ok(slices)
}

fn validate_common(
    parent_id: &str,
    quantity: i128,
    start_ts: u64,
    end_ts: u64,
    child_count: usize,
) -> Result<(), AlgoExecutionError> {
    if parent_id.trim().is_empty() {
        return Err(AlgoExecutionError::InvalidParent);
    }
    if end_ts <= start_ts {
        return Err(AlgoExecutionError::InvalidRange);
    }
    if quantity <= 0 {
        return Err(AlgoExecutionError::InvalidQuantity);
    }
    if child_count == 0 || child_count > MAX_ALGO_CHILDREN || child_count as i128 > quantity {
        return Err(AlgoExecutionError::InvalidChildCount);
    }
    Ok(())
}

fn mul_div_floor(a: u128, b: u128, divisor: u128) -> Result<u128, AlgoExecutionError> {
    if divisor == 0 {
        return Err(AlgoExecutionError::ArithmeticOverflow);
    }
    let whole = (a / divisor)
        .checked_mul(b)
        .ok_or(AlgoExecutionError::ArithmeticOverflow)?;
    let fraction = (a % divisor)
        .checked_mul(b)
        .ok_or(AlgoExecutionError::ArithmeticOverflow)?
        / divisor;
    whole
        .checked_add(fraction)
        .ok_or(AlgoExecutionError::ArithmeticOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twap_conserves_quantity_and_distributes_remainder_deterministically() {
        let plan = plan_twap("parent-7", 11, 10, 40, 4).unwrap();
        assert_eq!(
            plan.iter()
                .map(|slice| slice.quantity_raw)
                .collect::<Vec<_>>(),
            [3, 3, 3, 2]
        );
        assert_eq!(
            plan.iter()
                .map(|slice| slice.scheduled_ts)
                .collect::<Vec<_>>(),
            [10, 20, 30, 40]
        );
        assert!(plan.iter().all(|slice| slice.parent_id == "parent-7"));
    }

    #[test]
    fn vwap_uses_volume_weights_and_assigns_rounding_remainder_to_last_child() {
        let plan = plan_vwap(
            "p",
            10,
            100,
            300,
            &[
                VwapVolumeBucket {
                    ts: 100,
                    market_volume_raw: 1,
                },
                VwapVolumeBucket {
                    ts: 200,
                    market_volume_raw: 2,
                },
                VwapVolumeBucket {
                    ts: 300,
                    market_volume_raw: 3,
                },
            ],
        )
        .unwrap();
        assert_eq!(
            plan.iter()
                .map(|slice| slice.quantity_raw)
                .collect::<Vec<_>>(),
            [1, 3, 6]
        );
        assert_eq!(
            plan.iter().map(|slice| slice.quantity_raw).sum::<i128>(),
            10
        );
    }

    #[test]
    fn vwap_rejects_unordered_or_zero_volume_profiles() {
        let unordered = [
            VwapVolumeBucket {
                ts: 200,
                market_volume_raw: 1,
            },
            VwapVolumeBucket {
                ts: 100,
                market_volume_raw: 2,
            },
        ];
        assert_eq!(
            plan_vwap("p", 3, 100, 200, &unordered),
            Err(AlgoExecutionError::InvalidVolumeProfile)
        );
        let zero = [VwapVolumeBucket {
            ts: 100,
            market_volume_raw: 0,
        }];
        assert_eq!(
            plan_vwap("p", 1, 100, 200, &zero),
            Err(AlgoExecutionError::InvalidVolumeProfile)
        );
    }

    #[test]
    fn child_count_and_time_bounds_are_fail_closed() {
        assert_eq!(
            plan_twap("p", 2, 10, 10, 1),
            Err(AlgoExecutionError::InvalidRange)
        );
        assert_eq!(
            plan_twap("p", 1, 10, 20, 2),
            Err(AlgoExecutionError::InvalidChildCount)
        );
    }
}
