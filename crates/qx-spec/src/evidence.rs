//! 证据包（EvidenceBundle）：把测试网、故障演练与性能结果变成可复核证据。
//!
//! 规划口径（docs/qianxing-项目结构与GitHub竞品对比及优化方案-2026-10-06.md §6.2 / §7 P0）：
//! evidence bundle **不能保存密钥、完整订单敏感字段或用户隐私**，只保存脱敏配置摘要与
//! 事实摘要。因此本对象只有「摘要 + 计数 + 指针」，`redacted_config_digest` 与
//! `result_digest` 都是摘要而不是原文，`logs` 只登记路径。

use crate::{digest_json, CapabilityLevel, FoundationDocument, FoundationKind};
use serde::{Deserialize, Serialize};

pub const EVIDENCE_BUNDLE_SCHEMA_VERSION: u32 = 1;

/// 这份证据要证明的断言，以及它支撑到哪一级能力。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceClaim {
    pub summary: String,
    pub capability_level: CapabilityLevel,
}

/// 证据覆盖的时间窗口（epoch 毫秒）。`start_ms` 必须早于 `end_ms`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceWindow {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// 运行环境摘要：主机、运行时版本与网络（testnet/mainnet/simulated）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceEnvironment {
    pub host: String,
    pub runtime_version: String,
    pub network: String,
}

/// 一份可复核证据包。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceBundle {
    pub schema_version: u32,
    pub claim: EvidenceClaim,
    pub venue_id: String,
    pub window: EvidenceWindow,
    pub environment: EvidenceEnvironment,
    pub redacted_config_digest: String,
    pub logs: Vec<String>,
    pub fact_count: u64,
    pub result_digest: String,
    pub operator: String,
}

impl EvidenceBundle {
    /// 证据覆盖的网络取值域。生产证据只认 testnet/sandbox 与 mainnet 三类，其他一律拒绝。
    pub const NETWORKS: [&str; 3] = ["testnet", "sandbox", "mainnet"];
}

impl FoundationDocument for EvidenceBundle {
    const KIND: FoundationKind = FoundationKind::Evidence;
    const SCHEMA_VERSION: u32 = EVIDENCE_BUNDLE_SCHEMA_VERSION;

    fn from_json(payload: &str) -> Result<Self, String> {
        let document: Self =
            serde_json::from_str(payload).map_err(|error| format!("证据包解析失败: {error}"))?;
        document.validate()?;
        Ok(document)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != EVIDENCE_BUNDLE_SCHEMA_VERSION {
            return Err(format!(
                "证据包 schema_version={} 不受支持（本构建只认 {}）",
                self.schema_version, EVIDENCE_BUNDLE_SCHEMA_VERSION
            ));
        }
        if self.claim.summary.trim().is_empty() {
            return Err("证据包 claim.summary 不能为空".into());
        }
        if self.venue_id.trim().is_empty() {
            return Err("证据包 venue_id 是必需项".into());
        }
        if self.window.start_ms == 0 || self.window.start_ms >= self.window.end_ms {
            return Err("证据包 window 必须满足 0 < start_ms < end_ms".into());
        }
        if self.environment.host.trim().is_empty()
            || self.environment.runtime_version.trim().is_empty()
        {
            return Err("证据包 environment 必须写明 host 与 runtime_version".into());
        }
        if !Self::NETWORKS.contains(&self.environment.network.as_str()) {
            return Err(format!(
                "证据包 environment.network={} 不在 {} 内",
                self.environment.network,
                Self::NETWORKS.join(" / ")
            ));
        }
        if self.redacted_config_digest.trim().is_empty() || self.result_digest.trim().is_empty() {
            return Err("证据包必须给出 redacted_config_digest 与 result_digest".into());
        }
        if self.logs.is_empty() {
            return Err("证据包至少要登记一份日志路径".into());
        }
        if self.fact_count == 0 {
            return Err("证据包 fact_count 必须为正：没有事实的证据不构成证据".into());
        }
        if self.operator.trim().is_empty() {
            return Err("证据包必须写明 operator".into());
        }
        // 生产级证据（L4）只认 mainnet 窗口；沙盒证据只认 testnet/sandbox。
        let level = self.claim.capability_level;
        if level >= CapabilityLevel::L4 && self.environment.network != "mainnet" {
            return Err(format!(
                "证据包声明 {} 但 network={}：L4 证据必须来自 mainnet 窗口",
                level.as_str(),
                self.environment.network
            ));
        }
        if level == CapabilityLevel::L3 && self.environment.network == "mainnet" {
            return Err("证据包声明 L3 却来自 mainnet：L3 是沙盒级证据，生产证据应登记 L4".into());
        }
        Ok(())
    }

    fn identity(&self) -> String {
        format!("{}@{}", self.venue_id, self.claim.capability_level.as_str())
    }

    fn fingerprint(&self) -> Result<String, String> {
        digest_json(self)
    }

    fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|error| format!("证据包规范化失败: {error}"))
    }
}
