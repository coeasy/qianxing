//! 项目清单（ProjectManifest）：统一项目入口的声明式身份。
//!
//! 规划口径（docs/qianxing-项目结构与GitHub竞品对比及优化方案-2026-10-06.md §6.2）：
//! 项目清单替代用户直接手拼几十个 JSON。它只声明「这个项目用哪份运行时配置、哪几个
//! 数据集版本、哪几个策略、产物写到哪里」，不复制运行时配置本身，也不产生交易语义。

use crate::{digest_json, FoundationDocument, FoundationKind};
use serde::{Deserialize, Serialize};

pub const PROJECT_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// 规划 §16.2 的场景 profile 清单：profile 只生成自包含项目，不代表已具备真实 Venue 证据。
pub const PROJECT_PROFILES: [&str; 9] = [
    "ashare-research",
    "ashare-paper",
    "cn-futures-research",
    "cn-options-research",
    "global-equity-paper",
    "fx-paper",
    "crypto-paper",
    "crypto-testnet",
    "multi-venue-arb",
];

/// 策略语言取值域；与 `strategy new` 的三语言模板一一对应。
pub const PROJECT_STRATEGY_LANGUAGES: [&str; 3] = ["rust", "python", "cpp"];

/// 一个项目引用的数据集版本。身份 = (dataset_id, version)，与 `qx-data` 的目录口径一致。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetRef {
    pub dataset_id: String,
    pub version: String,
}

/// 一个项目引用的策略。`source` 是相对项目根的路径或内置策略名。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyRef {
    pub strategy_id: String,
    pub language: String,
    pub source: String,
}

/// 项目清单：`project init` 生成、`plan project` 校验的同一份对象。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectManifest {
    pub schema_version: u32,
    pub project_id: String,
    pub profile: String,
    pub runtime: String,
    pub artifact_root: String,
    pub datasets: Vec<DatasetRef>,
    pub strategies: Vec<StrategyRef>,
}

impl ProjectManifest {
    /// 项目身份只有一个来源：`project_id`。`datasets`/`strategies` 只作引用，不承担身份。
    pub fn project_id(&self) -> &str {
        &self.project_id
    }
}

impl FoundationDocument for ProjectManifest {
    const KIND: FoundationKind = FoundationKind::Project;
    const SCHEMA_VERSION: u32 = PROJECT_MANIFEST_SCHEMA_VERSION;

    fn from_json(payload: &str) -> Result<Self, String> {
        let document: Self =
            serde_json::from_str(payload).map_err(|error| format!("项目清单解析失败: {error}"))?;
        document.validate()?;
        Ok(document)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != PROJECT_MANIFEST_SCHEMA_VERSION {
            return Err(format!(
                "项目清单 schema_version={} 不受支持（本构建只认 {}）",
                self.schema_version, PROJECT_MANIFEST_SCHEMA_VERSION
            ));
        }
        if self.project_id.trim().is_empty() {
            return Err("项目清单 project_id 不能为空".into());
        }
        if !PROJECT_PROFILES.contains(&self.profile.as_str()) {
            return Err(format!(
                "项目清单 profile={} 不在场景清单内（可选：{}）",
                self.profile,
                PROJECT_PROFILES.join(" / ")
            ));
        }
        if self.runtime.trim().is_empty() || self.artifact_root.trim().is_empty() {
            return Err("项目清单 runtime 与 artifact_root 都是必需项".into());
        }
        if self.datasets.is_empty() {
            return Err("项目清单至少要引用一个数据集版本".into());
        }
        for dataset in &self.datasets {
            if dataset.dataset_id.trim().is_empty() || dataset.version.trim().is_empty() {
                return Err("项目清单 datasets 每项都要有 dataset_id 与 version".into());
            }
        }
        for strategy in &self.strategies {
            if strategy.strategy_id.trim().is_empty() || strategy.source.trim().is_empty() {
                return Err("项目清单 strategies 每项都要有 strategy_id 与 source".into());
            }
            if !PROJECT_STRATEGY_LANGUAGES.contains(&strategy.language.as_str()) {
                return Err(format!(
                    "项目清单策略 {} 的 language={} 不在 {} 内",
                    strategy.strategy_id,
                    strategy.language,
                    PROJECT_STRATEGY_LANGUAGES.join(" / ")
                ));
            }
        }
        Ok(())
    }

    fn identity(&self) -> String {
        self.project_id().to_string()
    }

    fn fingerprint(&self) -> Result<String, String> {
        digest_json(self)
    }

    fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|error| format!("项目清单规范化失败: {error}"))
    }
}
