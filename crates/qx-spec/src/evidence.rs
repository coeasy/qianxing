//! 统一证据包（EvidenceBundle）：同一份类型承载外部 venue 证据与本地运行复核证据。
//!
//! `generation` 明确区分两种证据来源：`venue` 证明 testnet/sandbox/mainnet 窗口事实，
//! `run` 聚合一次回测/Paper 运行的输入、产物、验证和离线复算指引。两代字段不互相冒充；
//! 两种 v1 旧 JSON 形状与统一包 v2 都在读入口显式迁入版本 3，写侧只产出本文件定义的统一形状。

use crate::run_evidence::RunEvidencePayload;
use crate::{digest_json, CapabilityLevel, FoundationDocument, FoundationKind};
use serde::{Deserialize, Serialize};

pub const EVIDENCE_BUNDLE_SCHEMA_VERSION: u32 = 3;
pub const LEGACY_EVIDENCE_BUNDLE_SCHEMA_VERSION: u32 = 1;
pub const LEGACY_RUN_EVIDENCE_SCHEMA_VERSION: u32 = 1;
pub const PREVIOUS_EVIDENCE_BUNDLE_SCHEMA_VERSION: u32 = 2;

fn unsupported_evidence_bundle_version(version: u64) -> String {
    format!(
        "证据包 schema_version={} 不受支持（本构建只认 {}）",
        version, EVIDENCE_BUNDLE_SCHEMA_VERSION
    )
}

/// 这份 venue 证据要证明的断言，以及它支撑到哪一级能力。
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

/// 运行环境摘要：主机、运行时版本与网络（testnet/sandbox/mainnet）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceEnvironment {
    pub host: String,
    pub runtime_version: String,
    pub network: String,
}

/// venue 窗口代的证据字段。L3/L4 只能由这一代证明。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VenueEvidencePayload {
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

impl VenueEvidencePayload {
    /// 证据覆盖的网络取值域。生产证据只认 mainnet，沙盒证据只认 testnet/sandbox。
    pub const NETWORKS: [&str; 3] = ["testnet", "sandbox", "mainnet"];

    pub(crate) fn validate(&self) -> Result<(), String> {
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
        if self.logs.is_empty() || self.logs.iter().any(|path| path.trim().is_empty()) {
            return Err("证据包至少要登记一份非空日志路径".into());
        }
        if self.fact_count == 0 {
            return Err("证据包 fact_count 必须为正：没有事实的证据不构成证据".into());
        }
        if self.operator.trim().is_empty() {
            return Err("证据包必须写明 operator".into());
        }
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
}

/// 证据来源代。外部窗口与本地运行保持不同的字段集合和校验规则。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceGeneration {
    Venue(VenueEvidencePayload),
    Run(RunEvidencePayload),
}

/// 唯一的证据包文档与 schema。`generation` 是来源判别字段。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceBundle {
    pub schema_version: u32,
    pub generation: EvidenceGeneration,
}

impl EvidenceBundle {
    /// 把旧 venue/run v1 与统一 EvidenceBundle v2 JSON 升级为当前唯一形状。
    ///
    /// 两种旧格式都曾使用 schema_version=1，因此按互斥的顶层字段集合识别；缺失两类
    /// 身份键或同时包含两类身份键均为格式错误，不能按猜测迁移。
    pub fn migrate_legacy_evidence_bundle(payload: &str) -> Result<Self, String> {
        let mut value: serde_json::Value =
            serde_json::from_str(payload).map_err(|error| format!("旧证据包解析失败: {error}"))?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| "旧证据包必须是 JSON 对象".to_string())?;
        let legacy_version = object
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "旧证据包缺少整数 schema_version".to_string())?;
        if legacy_version != u64::from(LEGACY_EVIDENCE_BUNDLE_SCHEMA_VERSION)
            && legacy_version != u64::from(LEGACY_RUN_EVIDENCE_SCHEMA_VERSION)
            && legacy_version != u64::from(PREVIOUS_EVIDENCE_BUNDLE_SCHEMA_VERSION)
        {
            return Err(format!(
                "旧证据包 schema_version={legacy_version} 不属于已登记迁移来源"
            ));
        }
        if legacy_version == u64::from(PREVIOUS_EVIDENCE_BUNDLE_SCHEMA_VERSION) {
            object.insert(
                "schema_version".into(),
                serde_json::json!(EVIDENCE_BUNDLE_SCHEMA_VERSION),
            );
            let document: Self = serde_json::from_value(value)
                .map_err(|error| format!("旧统一证据包迁移失败: {error}"))?;
            document.validate()?;
            return Ok(document);
        }
        object.remove("schema_version");
        let is_run = object.contains_key("run");
        let is_venue = object.contains_key("claim") || object.contains_key("venue_id");
        if is_run == is_venue {
            return Err("旧证据包世代无法唯一判定：必须且只能有 run 或 venue claim".into());
        }
        let generation = if is_run {
            let run: RunEvidencePayload = serde_json::from_value(value)
                .map_err(|error| format!("旧运行证据包解析失败: {error}"))?;
            run.validate()?;
            EvidenceGeneration::Run(run)
        } else {
            let venue: VenueEvidencePayload = serde_json::from_value(value)
                .map_err(|error| format!("旧 venue 证据包解析失败: {error}"))?;
            venue.validate()?;
            EvidenceGeneration::Venue(venue)
        };
        Ok(Self {
            schema_version: EVIDENCE_BUNDLE_SCHEMA_VERSION,
            generation,
        })
    }

    pub fn venue(payload: VenueEvidencePayload) -> Self {
        Self {
            schema_version: EVIDENCE_BUNDLE_SCHEMA_VERSION,
            generation: EvidenceGeneration::Venue(payload),
        }
    }

    pub fn run(payload: RunEvidencePayload) -> Self {
        Self {
            schema_version: EVIDENCE_BUNDLE_SCHEMA_VERSION,
            generation: EvidenceGeneration::Run(payload),
        }
    }
}

impl FoundationDocument for EvidenceBundle {
    const KIND: FoundationKind = FoundationKind::Evidence;
    const SCHEMA_VERSION: u32 = EVIDENCE_BUNDLE_SCHEMA_VERSION;

    fn from_json(payload: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(payload).map_err(|error| format!("证据包解析失败: {error}"))?;
        let schema_version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "证据包缺少整数 schema_version".to_string())?;
        match schema_version {
            version if version == u64::from(EVIDENCE_BUNDLE_SCHEMA_VERSION) => {
                let document: Self = serde_json::from_value(value)
                    .map_err(|error| format!("证据包解析失败: {error}"))?;
                document.validate()?;
                Ok(document)
            }
            version
                if version == u64::from(LEGACY_EVIDENCE_BUNDLE_SCHEMA_VERSION)
                    || version == u64::from(LEGACY_RUN_EVIDENCE_SCHEMA_VERSION)
                    || version == u64::from(PREVIOUS_EVIDENCE_BUNDLE_SCHEMA_VERSION) =>
            {
                let document = Self::migrate_legacy_evidence_bundle(payload)?;
                document.validate()?;
                Ok(document)
            }
            version => Err(unsupported_evidence_bundle_version(version)),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != EVIDENCE_BUNDLE_SCHEMA_VERSION {
            return Err(format!(
                "证据包 schema_version={} 不受支持（本构建只认 {}）",
                self.schema_version, EVIDENCE_BUNDLE_SCHEMA_VERSION
            ));
        }
        match &self.generation {
            EvidenceGeneration::Venue(venue) => venue.validate(),
            EvidenceGeneration::Run(run) => run.validate(),
        }
    }

    fn identity(&self) -> String {
        match &self.generation {
            EvidenceGeneration::Venue(venue) => venue.identity(),
            EvidenceGeneration::Run(run) => format!(
                "{}@{}",
                run.run.run_id,
                run.verification.capability_level.as_str()
            ),
        }
    }

    fn fingerprint(&self) -> Result<String, String> {
        digest_json(self)
    }

    fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|error| format!("证据包规范化失败: {error}"))
    }
}
