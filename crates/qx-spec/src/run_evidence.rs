//! 运行证据包（RunEvidenceBundle）：把一次**回测/Paper 运行**的全部可复核事实聚成一份文档。
//!
//! 规划口径（QX-DEV-PLAN-2026-10-10 阶段 1 / T1-1，退出门 G1 第一条「同一运行可由 RunManifest
//! **离线复算**」）：本对象**只聚合与校验，不重算任何指标**。它把散在各产物里的身份搬进同一份
//! 文档——RunManifest（代码/配置/数据/随机性/结果指纹）、summary / equity / fills 三份产物指针、
//! 数据指纹与质量报告、配置与策略身份、模型假设、软件构建身份、验证级别、**未验证清单**、
//! 以及**离线复算指引**——然后在 `validate` 里逐条做**交叉一致性**核对。
//!
//! 三条刻意写死的纪律，都是「改错了也不红」那一类故障的堵口：
//! ① **不许出现空洞的未验证清单**：`unverified` 必须非空。一份声称「没有任何未验证项」的证据包
//!    就是在盖单一 `verified` 标签，而那正是本计划（T1-7 / 复核 §2.2-C）明令禁止的形态。
//! ② **摘要未核的证据包不成立**：`artifact_digests_verified` 必须为 true。没有重算过产物 SHA-256
//!    的证据包只是「引用了一堆路径」。
//! ③ **本地运行证据不得冒充沙盒/生产级**：`capability_level` 只许 L0/L1/L2。L3/L4 是沙盒/生产档，
//!    必须由带真实 venue 窗口的 `EvidenceBundle` 支撑——两者不是同一种证据，不许互相顶替。
//!
//! 四格是**交叉一致性**：`dataset.composed_fingerprint`、`identity.config_digest`、
//! `identity.strategy_version`、`build.{code_commit,runtime_version}` 都必须与 `run` 块逐字相等。
//! 「产物身份与数据指纹各说各话」是本仓已经栽过的那类漂移，这里把它变成会拒的判据。
//!
//! 两处**刻意分开**的指纹（写这一格之前先看过真产物，不然就会写成一条永远拒的判据）：
//! `dataset.content_fingerprint` 是「这份输入文件是什么」（摘要 `input.fingerprint`，纯内容哈希），
//! 而 `run.data_fingerprint` 是「这次运行认哪份数据」（RunManifest 的**合成**身份，形如
//! `barframe:<内容哈希>` 或 `dataset-bundle:<bundle 指纹>`）。两者不是同一个值，硬写等号会让每一份
//! 真产物都被拒；这里让 `dataset` 同时登记两者，并把等号立在 `composed_fingerprint` 那一格上。
//!
//! 另一处**如实记空**：`dataset.quality_report` 是 `Option`。本仓今天只有 `data-validate` 那条
//! 只读诊断链会产出 `DatasetQualityReport`（T1-6 才把它铺开），回测链的输入身份是 v1 清单、不带
//! 质量报告。一份「全 0 的假报告」会被读成「核过了，没有重复、没有缺口」——比 `null` 危险得多。

use crate::run_record::{ArtifactRef, ReplayVerdict};
use crate::{digest_json, CapabilityLevel, FoundationDocument, FoundationKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const RUN_EVIDENCE_SCHEMA_VERSION: u32 = 1;

/// 离线复算至少要能拿到的那几份产物。少一份，「由 RunManifest 离线复算」这句话就落不了地。
pub const RUN_EVIDENCE_REQUIRED_ARTIFACTS: [&str; 4] =
    ["run_manifest", "summary", "equity", "fills"];

/// 一次运行的身份：RunManifest 的**逐字聚合**（搬运，不重算）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvidenceRun {
    pub run_id: String,
    pub code_commit: String,
    pub runtime_version: String,
    pub config_hash: String,
    pub data_fingerprint: String,
    pub input_components: BTreeMap<String, String>,
    pub clock_start: u64,
    pub clock_end: u64,
    pub global_seed: u64,
    pub determinism_mode: bool,
    pub result_hash: String,
    pub strategy_version: String,
    pub instrument_spec_version: String,
    pub model_fingerprint: String,
    pub input_event_hash: String,
    pub output_event_hash: String,
}

/// 数据质量报告：与 `qx_data::DatasetQualityReport` 同形状，**原样聚合**，不在此重算。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvidenceQualityReport {
    pub duplicate_rows: u64,
    pub out_of_order_rows: u64,
    pub missing_intervals: u64,
    pub timezone: String,
    pub corporate_action_coverage: bool,
    pub usable_tiers: Vec<String>,
}

/// 数据适用性：输入形状与身份 + 这一轮实际消费的样本数 + （可能缺席的）质量报告。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvidenceDataset {
    /// 输入形状：`barframe` / `depth-frame`。
    pub kind: String,
    pub dataset_id: String,
    pub version: String,
    /// 这份输入文件的**内容**指纹（摘要 `input.fingerprint`）。
    pub content_fingerprint: String,
    /// 这次运行认的**合成**数据身份，必须与 `run.data_fingerprint` 逐字相等。
    pub composed_fingerprint: String,
    /// 这一轮实际消费的样本数（摘要 `bars`）。它是运行事实，不等于数据集自身的行数。
    pub row_count: u64,
    /// 复核时按它重读同一份输入。
    pub path: String,
    /// 质量报告：这一档输入带一份就原样聚合，没有就如实写 `null`（见模块文档末段）。
    pub quality_report: Option<RunEvidenceQualityReport>,
}

/// 配置与策略身份：这份结果到底是「哪套配置跑哪个策略」跑出来的。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvidenceIdentity {
    pub strategy_id: String,
    pub strategy_version: String,
    pub instrument: String,
    pub config_digest: String,
}

/// 一条模型假设。`source` 是「这个取值从哪来」——空 source 等于把假设写成事实。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvidenceAssumption {
    pub name: String,
    pub value: String,
    pub source: String,
}

/// 软件构建身份：跑出这份结果的是哪一个二进制。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvidenceBuild {
    /// 与 `run.runtime_version` 逐字相等（RunManifest 记录的那一格运行/模式版本）。
    pub runtime_version: String,
    pub code_commit: String,
    /// `release` / `debug`，来自编译期。
    pub profile: String,
    /// `arch-os`，来自编译期。
    pub target_triple: String,
}

/// 验证级别与已核事实。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvidenceVerification {
    pub capability_level: CapabilityLevel,
    pub replay_verdict: ReplayVerdict,
    pub artifact_digests_verified: bool,
    pub verified_artifact_count: u64,
}

/// 离线复算指引：写到「拿哪几份输入、按什么顺序、期望得到什么」这一级。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvidenceRecompute {
    pub steps: Vec<String>,
    pub inputs: Vec<String>,
    pub expected_result_hash: String,
}

/// 一份运行证据包。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvidenceBundle {
    pub schema_version: u32,
    pub run: RunEvidenceRun,
    pub artifacts: Vec<ArtifactRef>,
    pub dataset: RunEvidenceDataset,
    pub identity: RunEvidenceIdentity,
    pub assumptions: Vec<RunEvidenceAssumption>,
    pub build: RunEvidenceBuild,
    pub verification: RunEvidenceVerification,
    pub unverified: Vec<String>,
    pub recompute: RunEvidenceRecompute,
    pub operator: String,
}

/// 一份非空字符串清单里的空项下标；用于把「留空被读成没有」这一类堵住。
fn blank_entries(values: &[String]) -> Vec<usize> {
    values
        .iter()
        .enumerate()
        .filter(|(_, value)| value.trim().is_empty())
        .map(|(index, _)| index)
        .collect()
}

impl RunEvidenceBundle {
    /// 本对象接受的能力档上限：L3/L4 属于沙盒/生产证据，由 `EvidenceBundle` 承担。
    pub const MAX_LOCAL_LEVEL: CapabilityLevel = CapabilityLevel::L2;

    fn check_run(&self) -> Result<(), String> {
        let required = [
            ("run.run_id", &self.run.run_id),
            ("run.code_commit", &self.run.code_commit),
            ("run.runtime_version", &self.run.runtime_version),
            ("run.config_hash", &self.run.config_hash),
            ("run.data_fingerprint", &self.run.data_fingerprint),
            ("run.result_hash", &self.run.result_hash),
            ("run.strategy_version", &self.run.strategy_version),
            (
                "run.instrument_spec_version",
                &self.run.instrument_spec_version,
            ),
            ("run.model_fingerprint", &self.run.model_fingerprint),
            ("run.input_event_hash", &self.run.input_event_hash),
            ("run.output_event_hash", &self.run.output_event_hash),
        ];
        if let Some((field, _)) = required.iter().find(|(_, value)| value.trim().is_empty()) {
            return Err(format!("运行证据包字段不能为空: {field}"));
        }
        if self
            .run
            .input_components
            .iter()
            .any(|(kind, fingerprint)| kind.trim().is_empty() || fingerprint.trim().is_empty())
        {
            return Err("运行证据包 run.input_components 不能包含空键或空指纹".into());
        }
        if self.run.clock_start > self.run.clock_end {
            return Err("运行证据包 run 时钟范围非法".into());
        }
        Ok(())
    }

    fn check_artifacts(&self) -> Result<(), String> {
        if self.artifacts.is_empty() {
            return Err("运行证据包至少要登记一份产物".into());
        }
        for artifact in &self.artifacts {
            if artifact.name.trim().is_empty()
                || artifact.path.trim().is_empty()
                || artifact.digest.trim().is_empty()
            {
                return Err(format!(
                    "运行证据包产物 {} 必须写明 name / path / digest",
                    artifact.name
                ));
            }
        }
        // 「由 RunManifest 离线复算」的最小集：缺哪一份都复算不出来，所以缺哪一份都拒。
        for required in RUN_EVIDENCE_REQUIRED_ARTIFACTS {
            if !self
                .artifacts
                .iter()
                .any(|artifact| artifact.name == required)
            {
                return Err(format!("运行证据包缺少离线复算必需的产物: {required}"));
            }
        }
        Ok(())
    }

    fn check_dataset(&self) -> Result<(), String> {
        for (field, value) in [
            ("dataset.kind", &self.dataset.kind),
            ("dataset.dataset_id", &self.dataset.dataset_id),
            ("dataset.version", &self.dataset.version),
            (
                "dataset.content_fingerprint",
                &self.dataset.content_fingerprint,
            ),
            (
                "dataset.composed_fingerprint",
                &self.dataset.composed_fingerprint,
            ),
            ("dataset.path", &self.dataset.path),
        ] {
            if value.trim().is_empty() {
                return Err(format!("运行证据包字段不能为空: {field}"));
            }
        }
        // 质量报告**缺席**是合法状态（这一档输入没有报告），但**带了一份空报告**不合法：
        // `usable_tiers` 为空等于说「没有任何一档可用」，那是一句该被拒的断言。
        if let Some(report) = &self.dataset.quality_report {
            if report.timezone.trim().is_empty() || report.usable_tiers.is_empty() {
                return Err(
                    "运行证据包 dataset.quality_report 若在场就必须写明 timezone 与非空 usable_tiers".into(),
                );
            }
        }
        Ok(())
    }

    /// 交叉一致性：合成数据指纹 / 配置摘要 / 策略版本 / 构建身份都必须与 `run` 块逐字相等。
    fn check_cross_references(&self) -> Result<(), String> {
        let pairs = [
            (
                "dataset.composed_fingerprint",
                &self.dataset.composed_fingerprint,
                &self.run.data_fingerprint,
            ),
            (
                "identity.config_digest",
                &self.identity.config_digest,
                &self.run.config_hash,
            ),
            (
                "identity.strategy_version",
                &self.identity.strategy_version,
                &self.run.strategy_version,
            ),
            (
                "build.code_commit",
                &self.build.code_commit,
                &self.run.code_commit,
            ),
            (
                "build.runtime_version",
                &self.build.runtime_version,
                &self.run.runtime_version,
            ),
        ];
        for (field, actual, expected) in pairs {
            if actual != expected {
                return Err(format!(
                    "运行证据包 {field}={actual} 与 run 块不一致（应为 {expected}）：产物身份与运行身份不许各说各话"
                ));
            }
        }
        Ok(())
    }

    fn check_evidence_honesty(&self) -> Result<(), String> {
        if self.verification.capability_level > Self::MAX_LOCAL_LEVEL {
            return Err(format!(
                "运行证据包声明 {} 超出本地运行证据的档位上限 {}：沙盒/生产档必须由带真实 venue 窗口的 EvidenceBundle 支撑",
                self.verification.capability_level.as_str(),
                Self::MAX_LOCAL_LEVEL.as_str()
            ));
        }
        if !self.verification.artifact_digests_verified {
            return Err("运行证据包必须已重算并核对产物摘要（artifact_digests_verified=false 只是引用了一堆路径）".into());
        }
        if self.verification.verified_artifact_count != self.artifacts.len() as u64 {
            return Err(format!(
                "运行证据包 verified_artifact_count={} 与登记的 {} 份产物不等",
                self.verification.verified_artifact_count,
                self.artifacts.len()
            ));
        }
        // 空清单 = 单一 verified 标签，本计划明令禁止。
        if self.unverified.is_empty() {
            return Err(
                "运行证据包 unverified 必须非空：没有任何未验证项的证据包是在盖单一 verified 标签"
                    .into(),
            );
        }
        if let Some(index) = blank_entries(&self.unverified).first() {
            return Err(format!("运行证据包 unverified 第 {} 项为空", index + 1));
        }
        Ok(())
    }

    fn check_assumptions(&self) -> Result<(), String> {
        if self.assumptions.is_empty() {
            return Err("运行证据包至少要写一条模型假设：撮合/费用/滑点/本金口径都不是事实".into());
        }
        for assumption in &self.assumptions {
            if assumption.name.trim().is_empty()
                || assumption.value.trim().is_empty()
                || assumption.source.trim().is_empty()
            {
                return Err(format!(
                    "运行证据包假设 {} 必须写明 name / value / source（空 source 等于把假设写成事实）",
                    assumption.name
                ));
            }
        }
        Ok(())
    }

    fn check_recompute(&self) -> Result<(), String> {
        if self.recompute.steps.is_empty() {
            return Err("运行证据包必须给出离线复算步骤：没有复算指引的证据包不算证据".into());
        }
        if let Some(index) = blank_entries(&self.recompute.steps).first() {
            return Err(format!(
                "运行证据包 recompute.steps 第 {} 项为空",
                index + 1
            ));
        }
        if self.recompute.inputs.is_empty() {
            return Err("运行证据包必须登记离线复算要用的输入".into());
        }
        if let Some(index) = blank_entries(&self.recompute.inputs).first() {
            return Err(format!(
                "运行证据包 recompute.inputs 第 {} 项为空",
                index + 1
            ));
        }
        if self.recompute.expected_result_hash != self.run.result_hash {
            return Err(format!(
                "运行证据包 recompute.expected_result_hash={} 与 run.result_hash={} 不一致：复算指引指的不是这次运行",
                self.recompute.expected_result_hash, self.run.result_hash
            ));
        }
        Ok(())
    }
}

impl FoundationDocument for RunEvidenceBundle {
    const KIND: FoundationKind = FoundationKind::RunEvidence;
    const SCHEMA_VERSION: u32 = RUN_EVIDENCE_SCHEMA_VERSION;

    fn from_json(payload: &str) -> Result<Self, String> {
        let document: Self = serde_json::from_str(payload)
            .map_err(|error| format!("运行证据包解析失败: {error}"))?;
        document.validate()?;
        Ok(document)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != RUN_EVIDENCE_SCHEMA_VERSION {
            return Err(format!(
                "运行证据包 schema_version={} 不受支持（本构建只认 {}）",
                self.schema_version, RUN_EVIDENCE_SCHEMA_VERSION
            ));
        }
        self.check_run()?;
        self.check_artifacts()?;
        self.check_dataset()?;
        self.check_cross_references()?;
        self.check_assumptions()?;
        self.check_evidence_honesty()?;
        self.check_recompute()?;
        if self.operator.trim().is_empty() {
            return Err("运行证据包必须写明 operator".into());
        }
        Ok(())
    }

    fn identity(&self) -> String {
        format!(
            "{}@{}",
            self.run.run_id,
            self.verification.capability_level.as_str()
        )
    }

    fn fingerprint(&self) -> Result<String, String> {
        digest_json(self)
    }

    fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|error| format!("运行证据包规范化失败: {error}"))
    }
}
