//! 模式登记册（SchemaRegistry）：统一管理 JSON、C ABI、Arrow、wire event 与兼容策略。
//!
//! 规划口径（docs/qianxing-项目结构与GitHub竞品对比及优化方案-2026-10-06.md §6.2）：
//! 每个 schema 有 id、版本、生产/消费方、兼容级别与 golden fixtures。登记册只描述
//! 「哪份契约在哪、谁产谁消、跨版本怎么兼容」，本身不复制契约正文；正文仍是
//! `schemas/<name>.json` 那唯一一份文本。

use crate::{digest_json, FoundationDocument, FoundationKind};
use serde::{Deserialize, Serialize};

pub const SCHEMA_REGISTRY_SCHEMA_VERSION: u32 = 1;

/// 契约的跨版本兼容级别。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaCompatibility {
    Backward,
    Forward,
    Full,
    None,
}

impl SchemaCompatibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            SchemaCompatibility::Backward => "backward",
            SchemaCompatibility::Forward => "forward",
            SchemaCompatibility::Full => "full",
            SchemaCompatibility::None => "none",
        }
    }
}

/// 登记册里的一条契约。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaEntry {
    pub schema_id: String,
    pub version: u32,
    pub path: String,
    pub producer: String,
    pub consumers: Vec<String>,
    pub compatibility: SchemaCompatibility,
    pub golden_fixtures: Vec<String>,
}

/// 模式登记册：把散落各处的契约收成一份可机器核对的台账。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaRegistry {
    pub schema_version: u32,
    pub registry_version: u32,
    pub entries: Vec<SchemaEntry>,
}

impl FoundationDocument for SchemaRegistry {
    const KIND: FoundationKind = FoundationKind::SchemaRegistry;
    const SCHEMA_VERSION: u32 = SCHEMA_REGISTRY_SCHEMA_VERSION;

    fn from_json(payload: &str) -> Result<Self, String> {
        let document: Self = serde_json::from_str(payload)
            .map_err(|error| format!("模式登记册解析失败: {error}"))?;
        document.validate()?;
        Ok(document)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_REGISTRY_SCHEMA_VERSION {
            return Err(format!(
                "模式登记册 schema_version={} 不受支持（本构建只认 {}）",
                self.schema_version, SCHEMA_REGISTRY_SCHEMA_VERSION
            ));
        }
        if self.registry_version == 0 {
            return Err("模式登记册 registry_version 必须为正".into());
        }
        if self.entries.is_empty() {
            return Err("模式登记册至少要登记一条契约".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &self.entries {
            if entry.schema_id.trim().is_empty() || entry.version == 0 {
                return Err("模式登记册每项都要有 schema_id 与正数 version".into());
            }
            if !seen.insert(entry.schema_id.as_str()) {
                return Err(format!("模式登记册 schema_id 重复：{}", entry.schema_id));
            }
            if entry.path.trim().is_empty() || entry.producer.trim().is_empty() {
                return Err(format!(
                    "模式登记册 {} 必须写明 path 与 producer",
                    entry.schema_id
                ));
            }
            if entry.consumers.is_empty() {
                return Err(format!("模式登记册 {} 至少要有一个消费者", entry.schema_id));
            }
            if entry.golden_fixtures.is_empty() {
                return Err(format!(
                    "模式登记册 {} 至少要有一份 golden fixture",
                    entry.schema_id
                ));
            }
            if entry.compatibility == SchemaCompatibility::None && entry.golden_fixtures.len() < 2 {
                return Err(format!(
                    "模式登记册 {} 声明兼容级别 {}，必须至少两份 fixture 证明跨版本差异",
                    entry.schema_id,
                    entry.compatibility.as_str()
                ));
            }
        }
        Ok(())
    }

    fn identity(&self) -> String {
        format!(
            "registry-v{} ({} entries)",
            self.registry_version,
            self.entries.len()
        )
    }

    fn fingerprint(&self) -> Result<String, String> {
        digest_json(self)
    }

    fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|error| format!("模式登记册规范化失败: {error}"))
    }
}
