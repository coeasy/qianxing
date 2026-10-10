//! 数据集清单 v2（DatasetManifestV2）：下载、校验、补洞与复现的统一身份。
//!
//! 规划口径（docs/qianxing-项目结构与GitHub竞品对比及优化方案-2026-10-06.md §6.2 / §7 P3）：
//! 数据集身份 = `dataset_id + version + fingerprint`，主源/备源、provider 版本、schema 版本
//! 与内容 hash 都要写进清单；**备源只做回填/交叉校验，不能静默替换主源**。
//!
//! v2 在 v1（`DatasetManifest`）之上增加 instrument、timezone、档位、质量报告与来源 lineage，
//! 且把 v1 作为嵌套子对象而不是 flatten——flatten 与 `deny_unknown_fields` 在 serde 里不兼容，
//! 嵌套才能既保住严格未知字段拒绝、又让 v1 身份逐字复用。

use crate::catalog::DatasetManifest;
use qx_core::Fnv1a;
use serde::{Deserialize, Serialize};

pub const DATASET_MANIFEST_V2_SCHEMA_VERSION: u32 = 1;

/// 数据集可用的行情档位。策略要求的档位与撮合模型必须匹配，缺档即拒。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatasetTier {
    Bar,
    L1,
    L2,
    L3,
}

impl DatasetTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            DatasetTier::Bar => "bar",
            DatasetTier::L1 => "l1",
            DatasetTier::L2 => "l2",
            DatasetTier::L3 => "l3",
        }
    }
}

/// 数据质量报告：回测前就该暴露的连续性问题。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetQualityReport {
    pub row_count: u64,
    pub duplicate_rows: u64,
    pub out_of_order_rows: u64,
    pub missing_intervals: u64,
    pub timezone: String,
    pub corporate_action_coverage: bool,
    pub usable_tiers: Vec<String>,
}

/// 来源 lineage：主源与备源分开登记，备源不得静默替换主源身份。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetSourceLineage {
    pub primary_source: String,
    #[serde(default)]
    pub backup_sources: Vec<String>,
}

/// 数据集清单 v2。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetManifestV2 {
    pub manifest_version: u32,
    pub dataset: DatasetManifest,
    pub instrument: String,
    pub timezone: String,
    pub tier: DatasetTier,
    pub provider_version: String,
    pub content_hash: String,
    pub quality_report: DatasetQualityReport,
    pub source_lineage: DatasetSourceLineage,
}

impl DatasetManifestV2 {
    pub fn from_json(payload: &str) -> Result<Self, String> {
        let manifest: Self = serde_json::from_str(payload)
            .map_err(|error| format!("数据集清单 v2 解析失败: {error}"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.manifest_version != DATASET_MANIFEST_V2_SCHEMA_VERSION {
            return Err(format!(
                "数据集清单 manifest_version={} 不受支持（本构建只认 {}）",
                self.manifest_version, DATASET_MANIFEST_V2_SCHEMA_VERSION
            ));
        }
        self.dataset.validate()?;
        if self.instrument.trim().is_empty() {
            return Err("数据集清单 v2 instrument 不能为空".into());
        }
        if self.timezone.trim().is_empty() || self.timezone != self.quality_report.timezone {
            return Err("数据集清单 v2 顶层 timezone 必须非空且与质量报告一致".into());
        }
        if self.provider_version.trim().is_empty() || self.content_hash.trim().is_empty() {
            return Err("数据集清单 v2 必须写明 provider_version 与 content_hash".into());
        }
        if self.quality_report.row_count == 0 {
            return Err("数据集清单 v2 质量报告 row_count 必须为正".into());
        }
        if !self
            .quality_report
            .usable_tiers
            .iter()
            .any(|tier| tier == self.tier.as_str())
        {
            return Err(format!(
                "数据集清单 v2 声明档位 {} 不在质量报告的 usable_tiers 内",
                self.tier.as_str()
            ));
        }
        if self.source_lineage.primary_source.trim().is_empty() {
            return Err("数据集清单 v2 必须写明主源 primary_source".into());
        }
        if self
            .source_lineage
            .backup_sources
            .iter()
            .any(|source| source == &self.source_lineage.primary_source)
        {
            return Err("数据集清单 v2 的备源不得与主源同名（备源只做回填/交叉校验）".into());
        }
        Ok(())
    }

    /// 数据集身份只有一处来源：v1 的 `dataset_id + version`。
    pub fn identity(&self) -> String {
        format!("{}@{}", self.dataset.dataset_id, self.dataset.version)
    }

    /// 只读告警：不改变身份，但报告里必须显示的口径提醒。
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if !self.source_lineage.backup_sources.is_empty() {
            warnings.push(format!(
                "备源 {} 只做回填/交叉校验，不覆盖主源身份 {}",
                self.source_lineage.backup_sources.join(" / "),
                self.source_lineage.primary_source
            ));
        }
        if self.quality_report.missing_intervals > 0 {
            warnings.push(format!(
                "数据存在 {} 个缺口区间，回测前须补洞或点名拒绝",
                self.quality_report.missing_intervals
            ));
        }
        if self.quality_report.out_of_order_rows > 0 || self.quality_report.duplicate_rows > 0 {
            warnings.push(format!(
                "数据含乱序 {} 行、重复 {} 行，已在质量报告中登记",
                self.quality_report.out_of_order_rows, self.quality_report.duplicate_rows
            ));
        }
        warnings
    }

    pub fn fingerprint(&self) -> Result<String, String> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("数据集清单 v2 序列化失败: {error}"))?;
        let mut hash = Fnv1a::new();
        hash.write_bytes(&bytes);
        Ok(format!("{:016x}", hash.finish()))
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self)
            .map_err(|error| format!("数据集清单 v2 规范化失败: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DatasetManifestV2 {
        DatasetManifestV2 {
            manifest_version: DATASET_MANIFEST_V2_SCHEMA_VERSION,
            dataset: DatasetManifest {
                dataset_id: "ashare.000001".into(),
                version: "snapshot-1".into(),
                source: "akshare".into(),
                fingerprint: "fp".into(),
                schema_version: 1,
                start_timestamp: 1,
                end_timestamp: 2,
            },
            instrument: "SSE.000001".into(),
            timezone: "Asia/Shanghai".into(),
            tier: DatasetTier::Bar,
            provider_version: "akshare-1.0".into(),
            content_hash: "content".into(),
            quality_report: DatasetQualityReport {
                row_count: 100,
                duplicate_rows: 0,
                out_of_order_rows: 0,
                missing_intervals: 0,
                timezone: "Asia/Shanghai".into(),
                corporate_action_coverage: true,
                usable_tiers: vec!["bar".into(), "l1".into()],
            },
            source_lineage: DatasetSourceLineage {
                primary_source: "akshare".into(),
                backup_sources: vec![],
            },
        }
    }

    #[test]
    fn dataset_v2_round_trips_and_is_stable() {
        let manifest = sample();
        manifest.validate().unwrap();
        let json = manifest.to_json().unwrap();
        let restored = DatasetManifestV2::from_json(&json).unwrap();
        assert_eq!(restored, manifest);
        assert_eq!(
            restored.fingerprint().unwrap(),
            manifest.fingerprint().unwrap()
        );
        assert_eq!(restored.identity(), "ashare.000001@snapshot-1");
        assert!(restored.warnings().is_empty());
    }

    #[test]
    fn dataset_v2_rejects_backup_that_shadows_primary_and_missing_tier() {
        let mut manifest = sample();
        manifest.source_lineage.backup_sources = vec!["akshare".into()];
        assert!(manifest.validate().is_err());
        let mut manifest = sample();
        manifest.tier = DatasetTier::L2;
        assert!(manifest.validate().is_err());
        let mut manifest = sample();
        manifest.quality_report.missing_intervals = 3;
        assert_eq!(manifest.warnings().len(), 1);
    }

    /// T1-2：版本不匹配必须给出**具名**拒绝——文案里同时点名「读到的版本」与「本构建只认的版本」。
    /// 只断言 `is_err()` 不够：换成一句泛泛的「解析失败」也照样绿，而读者拿到的是没有定位信息的
    /// 错误（这份清单是旧版本，还是文件本身坏了？）。
    #[test]
    fn dataset_manifest_v2_refuses_an_unsupported_manifest_version() {
        let mut manifest = sample();
        manifest.manifest_version = 7;
        let error = manifest.validate().expect_err("未来版本必须被拒");
        assert!(
            error.contains("manifest_version=7") && error.contains("只认 1"),
            "版本拒绝文案必须同时点名读到的版本与只认的版本；实际 {error}"
        );
    }
}
