//! 能力清单（CapabilityManifest）：Venue/Provider/Strategy/Storage 的能力与版本声明。
//!
//! 规划口径（docs/qianxing-项目结构与GitHub竞品对比及优化方案-2026-10-06.md §13.2 / §6.2）：
//! 「支持」不是一个模糊的 supported 标记，而是 L0–L4 五级，且**没有证据时不能手工写成
//! 已通过**。这里把这条纪律写成 `validate` 里的硬约束：声明 L3/L4 或把 `sandbox_tested`
//! 写成 true，都必须同时给出非空 `evidence`。

use crate::{digest_json, FoundationDocument, FoundationKind};
use serde::{Deserialize, Serialize};

pub const CAPABILITY_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// 规划 §13.2 的五级支持等级：L0 Schema/Research … L4 Controlled Production。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum CapabilityLevel {
    L0,
    L1,
    L2,
    L3,
    L4,
}

impl CapabilityLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            CapabilityLevel::L0 => "L0",
            CapabilityLevel::L1 => "L1",
            CapabilityLevel::L2 => "L2",
            CapabilityLevel::L3 => "L3",
            CapabilityLevel::L4 => "L4",
        }
    }
}

/// 一份 Venue/Provider 的能力与版本声明。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityManifest {
    pub schema_version: u32,
    pub venue_id: String,
    pub provider_id: String,
    pub level: CapabilityLevel,
    pub supported_products: Vec<String>,
    pub order_types: Vec<String>,
    pub data_tiers: Vec<String>,
    pub reconcile: bool,
    pub code_tested: bool,
    pub paper_tested: bool,
    pub sandbox_tested: bool,
    pub production_approved: bool,
    #[serde(default)]
    pub evidence: Vec<String>,
    pub strategy_api_version: u32,
    pub account_snapshot_version: u32,
}

impl FoundationDocument for CapabilityManifest {
    const KIND: FoundationKind = FoundationKind::Capability;
    const SCHEMA_VERSION: u32 = CAPABILITY_MANIFEST_SCHEMA_VERSION;

    fn from_json(payload: &str) -> Result<Self, String> {
        let document: Self =
            serde_json::from_str(payload).map_err(|error| format!("能力清单解析失败: {error}"))?;
        document.validate()?;
        Ok(document)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != CAPABILITY_MANIFEST_SCHEMA_VERSION {
            return Err(format!(
                "能力清单 schema_version={} 不受支持（本构建只认 {}）",
                self.schema_version, CAPABILITY_MANIFEST_SCHEMA_VERSION
            ));
        }
        if self.venue_id.trim().is_empty() || self.provider_id.trim().is_empty() {
            return Err("能力清单 venue_id 与 provider_id 都是必需项".into());
        }
        if self.supported_products.is_empty() || self.order_types.is_empty() {
            return Err("能力清单必须声明 supported_products 与 order_types".into());
        }
        if self.strategy_api_version == 0 || self.account_snapshot_version == 0 {
            return Err("能力清单必须声明 strategy_api_version 与 account_snapshot_version".into());
        }
        // 证据闸门：把「未拿到沙盒/生产记录就不能声明已通过」从文档承诺变成会拒的判据。
        if self.sandbox_tested && self.evidence.is_empty() {
            return Err(format!(
                "能力清单 {} 声明 sandbox_tested=true 但没有 evidence：未拿到外部沙盒记录不得声明已通过",
                self.venue_id
            ));
        }
        if self.production_approved && !self.sandbox_tested {
            return Err(format!(
                "能力清单 {} 声明 production_approved=true 但 sandbox_tested=false：等级不可越级",
                self.venue_id
            ));
        }
        if self.production_approved && self.evidence.is_empty() {
            return Err(format!(
                "能力清单 {} 声明 production_approved=true 但没有 evidence",
                self.venue_id
            ));
        }
        if self.level >= CapabilityLevel::L3 && !self.sandbox_tested {
            return Err(format!(
                "能力清单 {} 声明 level={} 但 sandbox_tested=false：L3 起必须有真实沙盒证据",
                self.venue_id,
                self.level.as_str()
            ));
        }
        if self.level >= CapabilityLevel::L4 && !self.production_approved {
            return Err(format!(
                "能力清单 {} 声明 level={} 但 production_approved=false",
                self.venue_id,
                self.level.as_str()
            ));
        }
        if self.reconcile && !self.order_types.iter().any(|kind| kind == "limit") {
            return Err(
                "能力清单声明 reconcile=true 但未声明限价单：对账口径缺少可核对的订单类型".into(),
            );
        }
        Ok(())
    }

    fn identity(&self) -> String {
        format!("{}@{}", self.venue_id, self.level.as_str())
    }

    fn fingerprint(&self) -> Result<String, String> {
        digest_json(self)
    }

    fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|error| format!("能力清单规范化失败: {error}"))
    }
}
