//! Content-addressed dataset snapshot metadata and execution admission checks.

use crate::{DatasetManifestV2, DatasetTier};
use serde::{Deserialize, Serialize};

pub const DATASET_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub const DATASET_SNAPSHOT_CONTEXT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetInstrumentRole {
    Index,
    Equity,
    Etf,
    Future,
    CryptoSpot,
    Fx,
    OtherTradable,
    Unknown,
}

impl DatasetInstrumentRole {
    pub fn is_tradable(self) -> bool {
        matches!(
            self,
            Self::Equity
                | Self::Etf
                | Self::Future
                | Self::CryptoSpot
                | Self::Fx
                | Self::OtherTradable
        )
    }

    pub fn is_compatible_with_product(self, product: qx_core::TradingProduct) -> bool {
        match self {
            Self::Index | Self::Unknown => false,
            Self::Equity | Self::Etf | Self::CryptoSpot => matches!(
                product,
                qx_core::TradingProduct::Spot | qx_core::TradingProduct::Margin
            ),
            Self::Future => matches!(
                product,
                qx_core::TradingProduct::Future | qx_core::TradingProduct::Perpetual
            ),
            Self::Fx => matches!(
                product,
                qx_core::TradingProduct::Spot
                    | qx_core::TradingProduct::Margin
                    | qx_core::TradingProduct::Future
            ),
            Self::OtherTradable => !matches!(product, qx_core::TradingProduct::Option),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetTimestampSemantics {
    BarOpen,
    BarClose,
    EventTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorporateActionPolicy {
    Adjusted,
    Unadjusted,
    NotApplicable,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetCoverageGapV1 {
    pub start_ts_ms: u64,
    pub end_ts_ms: u64,
}

/// Capture metadata supplied by the data owner when publishing a frozen snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetSnapshotContextV1 {
    pub schema_version: u32,
    pub instrument_role: DatasetInstrumentRole,
    pub captured_at_ms: u64,
    pub frequency_ms: u64,
    pub timestamp_semantics: DatasetTimestampSemantics,
    pub price_precision: u32,
    pub quantity_precision: u32,
    pub price_unit: String,
    pub quantity_unit: String,
    pub calendar_id: String,
    pub calendar_version: String,
    pub gaps: Vec<DatasetCoverageGapV1>,
    pub corporate_action_policy: CorporateActionPolicy,
}

/// Immutable capture metadata; paths and mutable provider handles are deliberately excluded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetSnapshotV1 {
    pub schema_version: u32,
    pub manifest: DatasetManifestV2,
    pub instrument_role: DatasetInstrumentRole,
    pub captured_at_ms: u64,
    pub frequency_ms: u64,
    pub timestamp_semantics: DatasetTimestampSemantics,
    pub price_precision: u32,
    pub quantity_precision: u32,
    pub price_unit: String,
    pub quantity_unit: String,
    pub coverage_start_ts_ms: u64,
    pub coverage_end_ts_ms: u64,
    pub calendar_id: String,
    pub calendar_version: String,
    pub gaps: Vec<DatasetCoverageGapV1>,
    pub corporate_action_policy: CorporateActionPolicy,
}

/// Parse either a legacy v2 quality manifest or its metadata-bound snapshot envelope.
pub fn parse_dataset_manifest_or_snapshot(
    payload: &str,
) -> Result<(DatasetManifestV2, Option<DatasetSnapshotV1>), String> {
    let document: serde_json::Value = serde_json::from_str(payload)
        .map_err(|error| format!("dataset manifest/snapshot JSON is invalid: {error}"))?;
    if document.get("manifest").is_some() && document.get("instrument_role").is_some() {
        let snapshot = DatasetSnapshotV1::from_json(payload)?;
        Ok((snapshot.manifest.clone(), Some(snapshot)))
    } else {
        Ok((DatasetManifestV2::from_json(payload)?, None))
    }
}

impl DatasetSnapshotV1 {
    pub fn from_manifest_and_context(
        manifest: DatasetManifestV2,
        context: DatasetSnapshotContextV1,
    ) -> Result<Self, String> {
        if context.schema_version != DATASET_SNAPSHOT_CONTEXT_SCHEMA_VERSION {
            return Err(format!(
                "unsupported dataset snapshot context schema_version {}; expected {}",
                context.schema_version, DATASET_SNAPSHOT_CONTEXT_SCHEMA_VERSION
            ));
        }
        let snapshot = Self {
            schema_version: DATASET_SNAPSHOT_SCHEMA_VERSION,
            coverage_start_ts_ms: manifest.dataset.start_timestamp,
            coverage_end_ts_ms: manifest.dataset.end_timestamp,
            manifest,
            instrument_role: context.instrument_role,
            captured_at_ms: context.captured_at_ms,
            frequency_ms: context.frequency_ms,
            timestamp_semantics: context.timestamp_semantics,
            price_precision: context.price_precision,
            quantity_precision: context.quantity_precision,
            price_unit: context.price_unit,
            quantity_unit: context.quantity_unit,
            calendar_id: context.calendar_id,
            calendar_version: context.calendar_version,
            gaps: context.gaps,
            corporate_action_policy: context.corporate_action_policy,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn from_json(payload: &str) -> Result<Self, String> {
        let snapshot: Self = serde_json::from_str(payload)
            .map_err(|error| format!("dataset snapshot JSON is invalid: {error}"))?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != DATASET_SNAPSHOT_SCHEMA_VERSION {
            return Err(format!(
                "unsupported dataset snapshot schema_version {}; expected {}",
                self.schema_version, DATASET_SNAPSHOT_SCHEMA_VERSION
            ));
        }
        self.manifest.validate()?;
        match (self.manifest.tier, self.timestamp_semantics) {
            (
                DatasetTier::Bar,
                DatasetTimestampSemantics::BarOpen | DatasetTimestampSemantics::BarClose,
            )
            | (
                DatasetTier::L1 | DatasetTier::L2 | DatasetTier::L3,
                DatasetTimestampSemantics::EventTime,
            ) => {}
            _ => {
                return Err("dataset timestamp semantics conflict with its market-data tier".into())
            }
        }
        match self.corporate_action_policy {
            CorporateActionPolicy::Adjusted
            | CorporateActionPolicy::Unadjusted
            | CorporateActionPolicy::NotApplicable
            | CorporateActionPolicy::Unknown => {}
        }
        if self.manifest.content_hash.len() != 64
            || !self
                .manifest
                .content_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("dataset snapshot content_hash must be a SHA-256 hex digest".into());
        }
        if self.captured_at_ms == 0 || self.frequency_ms == 0 {
            return Err("dataset snapshot capture time and frequency must be positive".into());
        }
        if self
            .manifest
            .quality_report
            .interval_ms
            .is_some_and(|interval| interval != self.frequency_ms)
        {
            return Err("dataset snapshot frequency does not match manifest interval_ms".into());
        }
        if self.coverage_start_ts_ms >= self.coverage_end_ts_ms
            || self.coverage_start_ts_ms != self.manifest.dataset.start_timestamp
            || self.coverage_end_ts_ms != self.manifest.dataset.end_timestamp
        {
            return Err("dataset snapshot coverage must match its manifest bounds".into());
        }
        if self.price_precision > 18 || self.quantity_precision > 18 {
            return Err("dataset snapshot precision must be between 0 and 18".into());
        }
        if self.price_unit.trim().is_empty()
            || self.quantity_unit.trim().is_empty()
            || self.calendar_id.trim().is_empty()
            || self.calendar_version.trim().is_empty()
        {
            return Err("dataset snapshot units and calendar identity are required".into());
        }
        let mut previous_end = None;
        let mut missing_intervals = 0_u64;
        for gap in &self.gaps {
            if gap.start_ts_ms >= gap.end_ts_ms
                || gap.start_ts_ms < self.coverage_start_ts_ms
                || gap.end_ts_ms > self.coverage_end_ts_ms
                || previous_end.is_some_and(|end| gap.start_ts_ms < end)
            {
                return Err(
                    "dataset snapshot gaps must be ordered, bounded, and non-overlapping".into(),
                );
            }
            let gap_intervals = (gap.end_ts_ms - gap.start_ts_ms).div_ceil(self.frequency_ms);
            missing_intervals = missing_intervals
                .checked_add(gap_intervals)
                .ok_or_else(|| "dataset snapshot gap count overflow".to_owned())?;
            previous_end = Some(gap.end_ts_ms);
        }
        if self.manifest.quality_report.missing_intervals != missing_intervals {
            return Err("dataset snapshot gap list does not match its quality report".into());
        }
        Ok(())
    }

    /// Check whether this snapshot is internally consistent and bound to one replay instrument.
    /// Research-only roles such as Index remain valid here; execution admission is stricter.
    pub fn validate_for_replay(&self, instrument: &str) -> Result<(), String> {
        self.validate()?;
        if instrument.trim().is_empty() || self.manifest.instrument != instrument {
            return Err(format!(
                "dataset snapshot instrument {} does not match replay instrument {instrument}",
                self.manifest.instrument
            ));
        }
        if self.instrument_role == DatasetInstrumentRole::Unknown {
            return Err("dataset snapshot instrument role is unknown for replay".into());
        }
        if self.manifest.quality_report.duplicate_rows > 0
            || self.manifest.quality_report.out_of_order_rows > 0
        {
            return Err("dataset snapshot contains duplicate or out-of-order observations".into());
        }
        Ok(())
    }

    /// Check whether this frozen snapshot can support a requested executable interval.
    pub fn validate_for_execution(
        &self,
        requested_start_ts_ms: u64,
        requested_end_ts_ms: u64,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<(), String> {
        self.validate()?;
        if !self.instrument_role.is_tradable() {
            return Err(format!(
                "dataset instrument role {:?} is research-only and cannot authorize execution",
                self.instrument_role
            ));
        }
        if self.corporate_action_policy == CorporateActionPolicy::Unknown {
            return Err("dataset corporate-action policy is unknown".into());
        }
        if self.manifest.quality_report.duplicate_rows > 0
            || self.manifest.quality_report.out_of_order_rows > 0
        {
            return Err("dataset snapshot contains duplicate or out-of-order observations".into());
        }
        if requested_start_ts_ms >= requested_end_ts_ms
            || requested_start_ts_ms < self.coverage_start_ts_ms
            || requested_end_ts_ms > self.coverage_end_ts_ms
        {
            return Err("requested execution interval is outside dataset snapshot coverage".into());
        }
        if self.captured_at_ms > now_ms || now_ms.saturating_sub(self.captured_at_ms) > max_age_ms {
            return Err("dataset snapshot is stale or future-dated".into());
        }
        if self.gaps.iter().any(|gap| {
            gap.start_ts_ms < requested_end_ts_ms && gap.end_ts_ms > requested_start_ts_ms
        }) {
            return Err("requested execution interval intersects a dataset coverage gap".into());
        }
        Ok(())
    }

    /// Bind the declared dataset role to the exact instrument/product the engine will trade.
    pub fn validate_for_market_spec(
        &self,
        market: &qx_core::TradingInstrumentSpec,
        requested_start_ts_ms: u64,
        requested_end_ts_ms: u64,
        now_ms: u64,
        max_age_ms: u64,
    ) -> Result<(), String> {
        self.validate_for_execution(
            requested_start_ts_ms,
            requested_end_ts_ms,
            now_ms,
            max_age_ms,
        )?;
        if self.manifest.instrument != market.instrument.to_string() {
            return Err(format!(
                "dataset instrument {} does not match execution instrument {}",
                self.manifest.instrument, market.instrument
            ));
        }
        if !self
            .instrument_role
            .is_compatible_with_product(market.product)
        {
            return Err(format!(
                "dataset instrument role {:?} conflicts with execution product {:?}",
                self.instrument_role, market.product
            ));
        }
        Ok(())
    }

    pub fn content_sha256(&self) -> Result<String, String> {
        let payload = serde_json::to_vec(self)
            .map_err(|error| format!("dataset snapshot encoding failed: {error}"))?;
        Ok(crate::sha256_hex_bytes(&payload))
    }
}
