//! qx-spec：声明式规格对象层（地基对象）。
//!
//! 本 crate 只承载「身份/声明」文档——项目清单、数据集清单 v2、实验规格、运行记录、
//! 能力清单、证据包与模式登记册。它们描述「打算做什么、依据是什么、结果能否复核」，
//! **不是账簿事实**，也不产生订单、成交或资金语义；因此本 crate 不依赖 adapter、
//! storage、API 或任何具体策略语言，只用 serde 与 `qx-core` 的稳定哈希。
//!
//! 每个对象对应一份 JSON Schema（`schemas/<name>.json`）与一个 `*_SCHEMA_VERSION`
//! 常量；两侧一致性由 `tools/check_architecture.py` 的 `foundation_specs_check` 钉住，
//! 常量本身另在 `maturity/baseline.yaml` 冻结。`describe` 是 CLI `plan <kind> <file>`
//! 的唯一读入漏斗：任何对象都先按严格 schema 解析、再自洽校验、最后算出稳定指纹。

pub mod capability;
pub mod evidence;
pub mod experiment;
pub mod project;
pub mod run_evidence;
pub mod run_record;
pub mod schema_registry;

pub use capability::{CapabilityLevel, CapabilityManifest, CAPABILITY_MANIFEST_SCHEMA_VERSION};
pub use evidence::{
    EvidenceBundle, EvidenceClaim, EvidenceEnvironment, EvidenceWindow,
    EVIDENCE_BUNDLE_SCHEMA_VERSION,
};
pub use experiment::{ExperimentSpec, ParameterSpace, SplitPlan, EXPERIMENT_SPEC_SCHEMA_VERSION};
pub use project::{DatasetRef, ProjectManifest, StrategyRef, PROJECT_MANIFEST_SCHEMA_VERSION};
pub use run_evidence::{
    RunEvidenceAssumption, RunEvidenceBuild, RunEvidenceBundle, RunEvidenceDataset,
    RunEvidenceIdentity, RunEvidenceRecompute, RunEvidenceRun, RunEvidenceVerification,
    RUN_EVIDENCE_REQUIRED_ARTIFACTS, RUN_EVIDENCE_SCHEMA_VERSION,
};
pub use run_record::{ArtifactRef, ReplayVerdict, RunRecord, RunStatus, RUN_RECORD_SCHEMA_VERSION};
pub use schema_registry::{
    SchemaCompatibility, SchemaEntry, SchemaRegistry, SCHEMA_REGISTRY_SCHEMA_VERSION,
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

/// 地基规格对象的八种类型；`as_str` 的取值即 CLI `plan <kind>` 接受的写法。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FoundationKind {
    Project,
    Dataset,
    Experiment,
    RunRecord,
    RunEvidence,
    Capability,
    Evidence,
    SchemaRegistry,
}

impl FoundationKind {
    /// 帮助文本与错误文案共用的完整取值清单（顺序即打印顺序）。
    pub const ALL: [FoundationKind; 8] = [
        FoundationKind::Project,
        FoundationKind::Dataset,
        FoundationKind::Experiment,
        FoundationKind::RunRecord,
        FoundationKind::RunEvidence,
        FoundationKind::Capability,
        FoundationKind::Evidence,
        FoundationKind::SchemaRegistry,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            FoundationKind::Project => "project",
            FoundationKind::Dataset => "dataset",
            FoundationKind::Experiment => "experiment",
            FoundationKind::RunRecord => "run-record",
            FoundationKind::RunEvidence => "run-evidence",
            FoundationKind::Capability => "capability",
            FoundationKind::Evidence => "evidence",
            FoundationKind::SchemaRegistry => "schema-registry",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

/// 七种「住在 qx-spec 里」的规格对象共用的读写契约。
///
/// `DatasetManifestV2` 是第八种，但它属于数据平面（`qx-data`），本 crate 只依赖它、
/// 不给它加反向依赖，因此它按固有方法在 `describe` 里单独接进来，不实现本 trait。
pub trait FoundationDocument: Serialize + DeserializeOwned + Sized {
    const KIND: FoundationKind;
    const SCHEMA_VERSION: u32;
    fn from_json(payload: &str) -> Result<Self, String>;
    fn validate(&self) -> Result<(), String>;
    fn identity(&self) -> String;
    fn fingerprint(&self) -> Result<String, String>;
    fn to_json(&self) -> Result<String, String>;
}

/// 一次规格读取的结果：身份、版本、稳定指纹、告警与规范化正文。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentReadout {
    pub kind: String,
    pub identity: String,
    pub schema_version: u32,
    pub fingerprint: String,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub canonical_json: String,
}

/// 规格对象的稳定指纹：对规范化 JSON 取 FNV-1a，与内核同一套跨平台稳定哈希。
pub(crate) fn digest_json<T: Serialize>(value: &T) -> Result<String, String> {
    let bytes =
        serde_json::to_vec(value).map_err(|error| format!("规格对象序列化失败: {error}"))?;
    let mut hash = qx_core::Fnv1a::new();
    hash.write_bytes(&bytes);
    Ok(format!("{:016x}", hash.finish()))
}

/// 按类型名读入并校验一份规格对象（CLI `plan <kind> <file>` 的唯一入口）。
pub fn describe(kind: &str, payload: &str) -> Result<DocumentReadout, String> {
    let kind = FoundationKind::parse(kind).ok_or_else(|| {
        let known = FoundationKind::ALL.map(|kind| kind.as_str()).join(" / ");
        format!("未知规格类型 {kind}（可选：{known}）")
    })?;
    match kind {
        FoundationKind::Project => readout::<ProjectManifest>(kind, payload),
        FoundationKind::Dataset => dataset_readout(payload),
        FoundationKind::Experiment => readout::<ExperimentSpec>(kind, payload),
        FoundationKind::RunRecord => readout::<RunRecord>(kind, payload),
        FoundationKind::RunEvidence => readout::<RunEvidenceBundle>(kind, payload),
        FoundationKind::Capability => readout::<CapabilityManifest>(kind, payload),
        FoundationKind::Evidence => readout::<EvidenceBundle>(kind, payload),
        FoundationKind::SchemaRegistry => readout::<SchemaRegistry>(kind, payload),
    }
}

fn readout<T: FoundationDocument>(
    kind: FoundationKind,
    payload: &str,
) -> Result<DocumentReadout, String> {
    let document = T::from_json(payload)?;
    document.validate()?;
    Ok(DocumentReadout {
        kind: kind.as_str().to_string(),
        identity: document.identity(),
        schema_version: T::SCHEMA_VERSION,
        fingerprint: document.fingerprint()?,
        warnings: Vec::new(),
        canonical_json: document.to_json()?,
    })
}

fn dataset_readout(payload: &str) -> Result<DocumentReadout, String> {
    let manifest = qx_data::DatasetManifestV2::from_json(payload)?;
    Ok(DocumentReadout {
        kind: FoundationKind::Dataset.as_str().to_string(),
        identity: manifest.identity(),
        schema_version: qx_data::DATASET_MANIFEST_V2_SCHEMA_VERSION,
        fingerprint: manifest.fingerprint()?,
        warnings: manifest.warnings(),
        canonical_json: manifest.to_json()?,
    })
}

#[cfg(test)]
mod tests;
