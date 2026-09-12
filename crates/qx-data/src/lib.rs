//! qx-data: unified multi asset data infrastructure.
//!
//! Data providers are kept outside the runtime kernel. All external sources
//! must be converted into canonical schemas before entering research/runtime.

pub mod schema;
pub mod catalog;
pub mod validation;
pub mod provider;
pub mod storage;
pub mod pipeline;

pub use catalog::DatasetManifest;
pub use schema::{Bar, DataSchemaVersion};
pub use validation::ValidationReport;
pub use storage::{DataStorage, MemoryDataStorage};
pub use pipeline::{process_bars, DataPipelineReport};
