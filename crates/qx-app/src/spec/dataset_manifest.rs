use crate::error::{AppError, AppErrorCategory};
use serde::{Deserialize, Serialize};

pub const DATASET_MANIFEST_VALIDATION_SCHEMA_VERSION: u32 = 1;
pub const DEPTH_DATASET_MANIFEST_VALIDATION_SCHEMA_VERSION: u32 = 1;

/// Input for strict L1/L2 depth-frame quality diagnosis and manifest publication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepthDatasetManifestValidationSpec {
    pub schema_version: u32,
    pub frame_path: String,
    pub tier: qx_data::DatasetTier,
    pub interval_ms: u64,
    pub timezone: String,
    pub output_path: String,
}

impl DepthDatasetManifestValidationSpec {
    pub fn new(
        frame_path: impl Into<String>,
        tier: qx_data::DatasetTier,
        interval_ms: u64,
        timezone: impl Into<String>,
        output_path: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: DEPTH_DATASET_MANIFEST_VALIDATION_SCHEMA_VERSION,
            frame_path: frame_path.into(),
            tier,
            interval_ms,
            timezone: timezone.into(),
            output_path: output_path.into(),
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != DEPTH_DATASET_MANIFEST_VALIDATION_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "DepthDatasetManifestValidationSpec schema_version={} 本版本只认 {}",
                    self.schema_version, DEPTH_DATASET_MANIFEST_VALIDATION_SCHEMA_VERSION
                ),
            ));
        }
        for (field, value) in [
            ("frame_path", &self.frame_path),
            ("timezone", &self.timezone),
            ("output_path", &self.output_path),
        ] {
            if value.trim().is_empty() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("DepthDatasetManifestValidationSpec.{field} 不能为空"),
                ));
            }
        }
        if !matches!(
            self.tier,
            qx_data::DatasetTier::L1 | qx_data::DatasetTier::L2
        ) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "深度数据集清单只支持 L1 或 L2 档位",
            ));
        }
        if self.interval_ms == 0 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "interval-ms 必须为正整数",
            ));
        }
        Ok(())
    }
}

/// Input for tolerant BarFrame quality diagnosis and idempotent manifest publication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetManifestValidationSpec {
    pub schema_version: u32,
    pub frame_path: String,
    pub dataset_id: String,
    pub version: String,
    pub interval_ms: u64,
    pub timezone: String,
    pub output_path: String,
}

impl DatasetManifestValidationSpec {
    pub fn new(
        frame_path: impl Into<String>,
        dataset_id: impl Into<String>,
        version: impl Into<String>,
        interval_ms: u64,
        timezone: impl Into<String>,
        output_path: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: DATASET_MANIFEST_VALIDATION_SCHEMA_VERSION,
            frame_path: frame_path.into(),
            dataset_id: dataset_id.into(),
            version: version.into(),
            interval_ms,
            timezone: timezone.into(),
            output_path: output_path.into(),
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != DATASET_MANIFEST_VALIDATION_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "DatasetManifestValidationSpec schema_version={} 本版本只认 {}",
                    self.schema_version, DATASET_MANIFEST_VALIDATION_SCHEMA_VERSION
                ),
            ));
        }
        for (field, value) in [
            ("frame_path", &self.frame_path),
            ("dataset_id", &self.dataset_id),
            ("version", &self.version),
            ("timezone", &self.timezone),
            ("output_path", &self.output_path),
        ] {
            if value.trim().is_empty() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("DatasetManifestValidationSpec.{field} 不能为空"),
                ));
            }
        }
        if self.interval_ms == 0 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "interval-ms 必须为正整数",
            ));
        }
        Ok(())
    }
}
