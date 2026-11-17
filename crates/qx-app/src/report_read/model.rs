//! Versioned data-transfer models for verified research report reads.
//!
//! These wire types are owned separately from the storage reader and replay workflow so every
//! front-end can share the same DTO contract without coupling its model to file traversal.

use crate::{
    DatasetVerdict, MultiMarketBacktestOutcome, MultiMarketBacktestSpec,
    MultiMarketDepthBacktestOutcome, MultiMarketDepthBacktestSpec, VerificationResult,
};
use qx_provider::DatasetManifestV2;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const RESEARCH_RUN_REPORT_SCHEMA_VERSION: u32 = 1;
pub const PROVIDER_BACKTEST_REPORT_SCHEMA_VERSION_V1: u32 = 1;
pub const PROVIDER_DEPTH_BACKTEST_REPORT_SCHEMA_VERSION_V1: u32 = 1;
pub const EVENT_REPORT_PAGE_SCHEMA_VERSION: u32 = 1;
pub const MULTI_MARKET_EVENT_REPORT_SCHEMA_VERSION_V1: u32 = 1;

/// Stable identity for matching a replayed Filled fact to one row in `fills.csv`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EventFillIdentityV1 {
    pub ts: u64,
    pub order_id: u64,
    pub qty_raw: i128,
    pub price_raw: i128,
    pub fee_raw: i128,
    pub strategy_id: String,
}

/// A replay-verified page and the complete fill multiset derived from its EventLog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventReportPageV1 {
    pub schema_version: u32,
    pub total: u64,
    pub offset: u64,
    pub truncated: bool,
    pub rows: Vec<Value>,
    pub fill_counts: Vec<EventFillCountV1>,
}

/// Stream-verified shared-account EventLog facts for a multi-market report.
#[derive(Clone, Debug)]
pub struct VerifiedMultiMarketEventReportV1 {
    pub total: u64,
    pub offset: u64,
    pub truncated: bool,
    pub rows: Vec<Value>,
    pub fill_count: u64,
    pub fill_sequence_sha256: String,
    pub order_markets: BTreeMap<u64, String>,
    pub ledger: qx_core::Ledger,
}

/// Bounded JSON request for the replay-verified multi-market EventLog page reader.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiMarketEventReportReadRequestV1 {
    pub schema_version: u32,
    pub outcome: MultiMarketBacktestOutcome,
    #[serde(default)]
    pub page: Option<MultiMarketEventReportPageRequestV1>,
}

/// Bounded request for a shared-account multi-market L1/L2 EventLog report page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiMarketDepthEventReportReadRequestV1 {
    pub schema_version: u32,
    pub outcome: MultiMarketDepthBacktestOutcome,
    #[serde(default)]
    pub page: Option<MultiMarketEventReportPageRequestV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MultiMarketEventReportPageRequestV1 {
    pub offset: u64,
    pub limit: usize,
}

/// Versioned, bounded facts from a fully replayed multi-market EventLog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MultiMarketEventReportPageV1 {
    pub schema_version: u32,
    pub run_id: String,
    pub total: u64,
    pub offset: u64,
    pub truncated: bool,
    pub rows: Vec<Value>,
    pub fill_count: u64,
    pub fill_sequence_sha256: String,
    pub order_markets: BTreeMap<u64, String>,
    pub replayed_ledger_entries: u64,
}

/// Verified application inputs retained for API report projection after event replay.
///
/// `artifact` is bounded by `MAX_ARTIFACT_BYTES`; event rows inside `page` remain bounded by the
/// requested page limit. Keeping the already-verified spec and manifest here prevents adapters
/// from independently parsing another identity path.
pub struct MultiMarketEventReportReadModelV1 {
    pub artifact: Value,
    pub spec: MultiMarketBacktestSpec,
    pub manifest: qx_core::RunManifest,
    pub page: MultiMarketEventReportPageV1,
    pub ledger: qx_core::Ledger,
}

/// Verified inputs retained by an API adapter after multi-market Depth event replay.
pub struct MultiMarketDepthEventReportReadModelV1 {
    pub artifact: Value,
    pub spec: MultiMarketDepthBacktestSpec,
    pub manifest: qx_core::RunManifest,
    pub page: MultiMarketEventReportPageV1,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventFillCountV1 {
    pub identity: EventFillIdentityV1,
    pub count: usize,
}

/// Common bounded row set. Row values are strings to preserve CSV precision and old wire
/// behavior across the Rust, Python, C++ and Web consumers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResearchReportRowsV1 {
    pub total: u64,
    pub truncated: bool,
    pub rows: Vec<Value>,
}

/// Core report facts shared by every report entry point. More detailed event/bar/depth data is
/// attached by the corresponding presentation surface only after this verdict succeeds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResearchRunReportV1 {
    pub schema_version: u32,
    pub run_id: String,
    pub instrument: String,
    pub verification: VerificationResult,
    pub summary: Value,
    pub equity: ResearchReportRowsV1,
    pub fills: ResearchReportRowsV1,
}

/// Provider Bar report envelope. The nested core report remains identical to ordinary Bar
/// reports; the additional fields expose only the frozen, identity-bound provider manifest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderBacktestReportV1 {
    pub schema_version: u32,
    pub report: ResearchRunReportV1,
    pub dataset_verdict: DatasetVerdict,
    pub manifest: DatasetManifestV2,
    pub dataset_snapshot: qx_data::DatasetSnapshotV1,
}

/// Provider Tick/OrderBook report envelope with the frozen data identity verified against the
/// provider provenance carried by the outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderDepthBacktestReportV1 {
    pub schema_version: u32,
    pub report: ResearchRunReportV1,
    pub provider_id: String,
    pub provider_version: String,
    pub request_id: String,
    pub retry_chain: Vec<String>,
    pub source_hash: String,
    pub query_digest: String,
    pub depth_input_hash: String,
    pub manifest: DatasetManifestV2,
    pub dataset_snapshot: qx_data::DatasetSnapshotV1,
    pub dataset_snapshot_sha256: String,
}
