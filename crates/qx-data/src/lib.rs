//! qx-data: unified multi asset data infrastructure.
//!
//! Data providers are kept outside the runtime kernel. All external sources
//! must be converted into canonical schemas before entering research/runtime.

pub mod bar_manifest;
pub mod batch;
pub mod calendar_fingerprint;
pub mod catalog;
pub mod catalog_v2;
pub mod corporate_action;
pub mod fingerprint;
pub mod incremental;
pub mod ingestion;
pub mod pipeline;
pub mod provider;
pub mod registry;
pub mod resolver;
pub mod schema;
pub mod storage;
pub mod validation;

/// Stable provider version used for JSON BarFrame datasets across ingest, validation and replay.
pub const BARFRAME_DATASET_VERSION: &str = "barframe-json-v1";
pub const DEPTHFRAME_DATASET_VERSION: &str = "depth-frame-v1";

/// SHA-256 for exact source bytes, shared by manifest writers and independent readers.
pub fn sha256_hex_bytes(bytes: &[u8]) -> String {
    use ring::digest::{digest, SHA256};
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub use bar_manifest::manifest_from_bar_frame;
pub use batch::{load_bar_batch, BarBatchItem, BarRequest};
pub use calendar_fingerprint::calendar_component_fingerprint_from_json;
pub use catalog::{
    ArrowDatasetManifest, ArrowFieldManifest, DatasetBundleManifest, DatasetComponentFormat,
    DatasetComponentManifest, DatasetManifest, JsonDatasetBundleStore,
};
pub use catalog_v2::{
    DatasetManifestV2, DatasetQualityReport, DatasetSourceLineage, DatasetTier,
    DATASET_MANIFEST_V2_SCHEMA_VERSION,
};
pub use corporate_action::{CorporateAction, CorporateActionType};
pub use fingerprint::{fingerprint_bar_frame, fingerprint_bars};
pub use incremental::{merge_bars, IncrementalMergeReport};
pub use ingestion::{ingest_bars, IngestionReport, IngestionRequest};
pub use pipeline::{process_bars, DataPipelineReport};
pub use provider::{
    BarFrameContract, DataProvider, JsonBarFrameProvider, ProviderMetadata,
    BAR_FRAME_SCHEMA_VERSION,
};
pub use registry::{DatasetRegistrar, DatasetRegistry, JsonDatasetRegistry};
pub use resolver::{DatasetRef, DatasetResolver};
pub use schema::Bar;
pub use storage::{DataStorage, JsonFileDataStorage, MemoryDataStorage};
pub use validation::{validate_bars, ValidationReport};
