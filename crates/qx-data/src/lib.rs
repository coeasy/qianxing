//! qx-data: unified multi asset data infrastructure.
//!
//! Data providers are kept outside the runtime kernel. All external sources
//! must be converted into canonical schemas before entering research/runtime.

pub mod batch;
pub mod cache;
pub mod calendar;
pub mod catalog;
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

pub use batch::{load_bar_batch, BarBatchItem, BarRequest};
pub use cache::{CacheKey, DataCache};
pub use calendar::{TradingCalendar, TradingSession};
pub use catalog::{
    ArrowDatasetManifest, ArrowFieldManifest, DatasetBundleManifest, DatasetComponentFormat,
    DatasetComponentManifest, DatasetManifest, JsonDatasetBundleStore,
};
pub use corporate_action::{CorporateAction, CorporateActionType};
pub use fingerprint::fingerprint_bars;
pub use incremental::{merge_bars, IncrementalMergeReport};
pub use ingestion::{ingest_bars, IngestionReport, IngestionRequest};
pub use pipeline::{process_bars, DataPipelineReport};
pub use provider::{
    BarFrameContract, DataProvider, JsonBarFrameProvider, ProviderMetadata,
    BAR_FRAME_SCHEMA_VERSION,
};
pub use registry::{DatasetRegistrar, DatasetRegistry, JsonDatasetRegistry};
pub use resolver::{DatasetRef, DatasetResolver};
pub use schema::{Bar, DataSchemaVersion};
pub use storage::{DataStorage, JsonFileDataStorage, MemoryDataStorage};
pub use validation::{validate_bars, ValidationReport};
