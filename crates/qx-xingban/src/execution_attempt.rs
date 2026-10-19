use qx_core::Side;

/// Why one eligible order did or did not trade on a market observation.
/// These are execution facts; they do not alter event-log replay semantics.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionAttemptReason {
    Filled,
    PartiallyFilled,
    ModelPartialFill,
    BarParticipationPartial,
    ModelDeclined,
    LimitNotMarketable,
    ProbabilityMiss,
    NoCapacity,
    ParticipationLimitExhausted,
    QueueBaselineUnavailable,
    QueueAheadRemains,
    NoVisibleLiquidity,
    MarketHalted,
    SideBlocked,
    InvalidModelOutput,
}

/// One deterministic, reportable attempt to execute an order.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExecutionAttempt {
    pub ts: u64,
    pub order_id: u64,
    pub instrument: String,
    pub side: Side,
    pub requested_qty_raw: i128,
    pub filled_qty_raw: i128,
    pub remaining_qty_raw: i128,
    pub price_raw: Option<i128>,
    pub reason: ExecutionAttemptReason,
    /// Snapshot-visible quantity at the relevant price level, when the matcher has one.
    #[serde(default)]
    pub displayed_qty_raw: Option<i128>,
    /// Estimated quantity ahead of this order before processing the observation.
    #[serde(default)]
    pub queue_ahead_before_raw: Option<i128>,
    /// Estimated quantity ahead after applying the observation to the queue model.
    #[serde(default)]
    pub queue_ahead_after_raw: Option<i128>,
    /// Estimated traded quantity inferred from displayed-depth reductions. This is not tape data.
    #[serde(default)]
    pub estimated_volume_raw: Option<i128>,
    /// Visible same-price depth reduction observed since this order's previous queue snapshot.
    #[serde(default)]
    pub displayed_reduction_raw: Option<i128>,
    /// Estimated volume assigned to consuming queue ahead (not to this order's fill).
    #[serde(default)]
    pub queue_consumed_raw: Option<i128>,
}

/// Per-attempt liquidity evidence. `estimated_volume_raw` is deliberately separate from fills:
/// a depth reduction is only a queue model estimate, never a reported market trade.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct ExecutionLiquidityEvidence {
    pub displayed_qty_raw: Option<i128>,
    pub queue_ahead_before_raw: Option<i128>,
    pub queue_ahead_after_raw: Option<i128>,
    pub estimated_volume_raw: Option<i128>,
    pub displayed_reduction_raw: Option<i128>,
    pub queue_consumed_raw: Option<i128>,
}

/// Single evaluation result lets the matcher record the model's explanation without a second
/// call that could consume another random number or change the simulated execution.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FillEvaluation {
    pub fill: Option<(i128, i128)>,
    pub no_fill_reason: ExecutionAttemptReason,
}

impl FillEvaluation {
    pub fn filled(price_raw: i128, qty_raw: i128) -> Self {
        Self {
            fill: Some((price_raw, qty_raw)),
            no_fill_reason: ExecutionAttemptReason::ModelDeclined,
        }
    }

    pub fn declined(reason: ExecutionAttemptReason) -> Self {
        Self {
            fill: None,
            no_fill_reason: reason,
        }
    }
}
