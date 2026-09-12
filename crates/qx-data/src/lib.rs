//! qx-data: unified multi asset data infrastructure.
//!
//! Data providers are kept outside the runtime kernel. All external sources
//! must be converted into canonical schemas before entering research/runtime.

pub mod cache;
pub mod calendar;
pub mod catalog;
pub mod corporate_action;
pub mod incremental;
pub mod pipeline;
pub mod provider;
pub mod registry;
pub mod resolver;
pub mod schema;
pub mod storage;
pub mod validation;

pub use cache::{CacheKey, DataCache};
pub use calendar::{TradingCalendar, TradingSession};
pub use catalog::DatasetManifest;
pub use corporate_action::{CorporateAction, CorporateActionType};
pub use incremental::{merge_bars, IncrementalMergeReport};
pub use pipeline::{process_bars, DataPipelineReport};
pub use registry::DatasetRegistry;
pub use resolver::{DatasetRef, DatasetResolver};
pub use schema::{Bar, DataSchemaVersion};
pub use storage::{DataStorage, MemoryDataStorage};
pub use validation::{validate_bars, ValidationReport};
