//! 运行代证据字段：把一次**回测/Paper 运行**的可复核事实聚成统一 EvidenceBundle 的负载。
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
//!    必须由统一 EvidenceBundle 的 `venue` 世代与真实窗口支撑——两类证据不许互相顶替。
//!
//! 四格是**交叉一致性**：`dataset.composed_fingerprint`、`identity.config_digest`、
//! `identity.strategy_version`、`build.{code_commit,runtime_version}` 都必须与 `run` 块逐字相等。
//! 「产物身份与数据指纹各说各话」是本仓已经栽过的那类漂移，这里把它变成会拒的判据。
//!
//! `dataset.content_fingerprint` 是输入内容哈希，`run.data_fingerprint` 是 RunManifest 合成身份；
//! 两者分开登记，只有 `dataset.composed_fingerprint` 与 `run.data_fingerprint` 相等。
//! `dataset.quality_report` 可缺席：strategy/fast backtest 可绑定已校验的 v2 清单；未绑定时保留
//! `null`，不伪造全零报告，避免把「未核」读成「已核且无缺陷」。

pub use crate::run_evidence_fidelity::RunEvidenceFidelity;
use crate::run_record::{ArtifactRef, ReplayVerdict};
use crate::CapabilityLevel;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[path = "run_evidence_depth_components.rs"]
mod run_evidence_depth_components;
#[path = "run_evidence_quality_manifest.rs"]
mod run_evidence_quality_manifest;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
    pub timezone: String,
    pub corporate_action_coverage: bool,
    pub usable_tiers: Vec<String>,
}

/// 数据适用性：输入形状与身份 + 这一轮实际消费的样本数 + （可能缺席的）质量报告。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvidenceDataset {
    /// 输入形状：`barframe` / `depth-frame` / `multi-barframe` / 多市场 BarFrame/DepthFrame。
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
    /// 若回测绑定了 DatasetManifestV2，记录该清单的独立指纹。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_fingerprint: Option<String>,
    /// 单输入回测绑定的 DatasetManifestV2 文件；导出时搬入便携包并重定位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_manifest_path: Option<String>,
    /// 与 quality_manifest_path 成对出现，用于离线复核清单字节。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_manifest_sha256: Option<String>,
    /// 质量报告：这一档输入带一份就原样聚合，没有就如实写 `null`（见模块文档末段）。
    pub quality_report: Option<RunEvidenceQualityReport>,
    /// 组合数据集的组成输入。普通单输入包省略此字段，以保持既有 JSON 形状。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<RunEvidenceDatasetComponent>,
}

/// 组合回测的一条有序输入腿/市场。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvidenceDatasetComponent {
    pub role: String,
    pub dataset_id: String,
    pub version: String,
    pub content_fingerprint: String,
    pub row_count: u64,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_report: Option<RunEvidenceQualityReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_manifest_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_manifest_sha256: Option<String>,
}

/// 可复核的报告分析输入。它不参与原回测 result_hash，但随证据包一并搬运。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvidenceReportAnalysis {
    pub kind: String,
    pub input_path: String,
    pub input_sha256: String,
    pub input_fingerprint: String,
    pub instrument: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_input_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_input_sha256: Option<String>,
    pub analysis: String,
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

/// 运行代的证据负载；外层版本、世代判别与统一读写由 `EvidenceBundle` 承载。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvidencePayload {
    pub run: RunEvidenceRun,
    pub artifacts: Vec<ArtifactRef>,
    pub dataset: RunEvidenceDataset,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_analysis: Option<RunEvidenceReportAnalysis>,
    pub identity: RunEvidenceIdentity,
    pub assumptions: Vec<RunEvidenceAssumption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fidelity: Option<RunEvidenceFidelity>,
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

impl RunEvidencePayload {
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
        run_evidence_quality_manifest::validate_dataset(&self.dataset)?;
        match self.dataset.kind.as_str() {
            "multi-barframe" => {
                if self.dataset.components.len() != 2
                    || self.dataset.components[0].role != "primary"
                    || self.dataset.components[1].role != "reference"
                {
                    return Err(
                        "multi-barframe 必须按 primary、reference 顺序登记两条输入腿".into(),
                    );
                }
                if self.dataset.row_count != self.dataset.components[0].row_count {
                    return Err("multi-barframe row_count 必须等于 primary 输入行数".into());
                }
                for component in &self.dataset.components {
                    if component.dataset_id.trim().is_empty()
                        || component.version.trim().is_empty()
                        || component.content_fingerprint.trim().is_empty()
                        || component.path.trim().is_empty()
                        || component.row_count == 0
                    {
                        return Err(format!(
                            "运行证据包 dataset.components.{} 必须有身份、路径和正行数",
                            component.role
                        ));
                    }
                    let quality_fields = [
                        component.manifest_fingerprint.is_some(),
                        component.quality_report.is_some(),
                        component.quality_manifest_path.is_some(),
                        component.quality_manifest_sha256.is_some(),
                    ];
                    if quality_fields.iter().any(|present| *present)
                        && !quality_fields.iter().all(|present| *present)
                    {
                        return Err(format!(
                            "运行证据包 dataset.components.{} 的质量清单字段必须成套出现",
                            component.role
                        ));
                    }
                }
                let [primary, reference] = self.dataset.components.as_slice() else {
                    unreachable!("multi-barframe length checked above")
                };
                let primary_has_manifest = primary.manifest_fingerprint.is_some();
                let reference_has_manifest = reference.manifest_fingerprint.is_some();
                if primary_has_manifest != reference_has_manifest
                    || (primary_has_manifest
                        && (primary.quality_report.is_none() || reference.quality_report.is_none()))
                {
                    return Err(
                        "multi-barframe 清单身份必须同时为两条腿提供 fingerprint 与 quality_report"
                            .into(),
                    );
                }
                for component in &self.dataset.components {
                    if let Some(report) = &component.quality_report {
                        if report.timezone.trim().is_empty()
                            || report.usable_tiers.is_empty()
                            || report.interval_ms == Some(0)
                        {
                            return Err(format!(
                                "运行证据包 dataset.components.{}.quality_report 必须声明时区、有效档位与正 interval_ms（若在场）",
                                component.role
                            ));
                        }
                    }
                }
            }
            "multi-market-barframe" => {
                if self.dataset.components.len() < 3 {
                    return Err("multi-market-barframe 至少要登记三个 market 组成输入".into());
                }
                let mut roles = std::collections::BTreeSet::new();
                let mut total_rows = 0_u64;
                for component in &self.dataset.components {
                    if !component.role.starts_with("market:")
                        || component.role["market:".len()..].trim().is_empty()
                        || !roles.insert(component.role.as_str())
                    {
                        return Err(format!(
                            "multi-market-barframe 组成输入 role 必须是唯一的 market:<instrument>: {}",
                            component.role
                        ));
                    }
                    if component.dataset_id.trim().is_empty()
                        || component.version.trim().is_empty()
                        || component.content_fingerprint.trim().is_empty()
                        || component.path.trim().is_empty()
                        || component.row_count == 0
                    {
                        return Err(format!(
                            "运行证据包 dataset.components.{} 必须有身份、路径和正行数",
                            component.role
                        ));
                    }
                    total_rows = total_rows
                        .checked_add(component.row_count)
                        .ok_or("multi-market-barframe row_count 总和溢出")?;
                }
                if self.dataset.row_count != total_rows {
                    return Err(format!(
                        "multi-market-barframe row_count 必须等于各市场行数总和: 声明={} 实际={total_rows}",
                        self.dataset.row_count
                    ));
                }
            }
            "multi-market-depthframe" => {
                run_evidence_depth_components::validate(&self.dataset)?;
            }
            _ if !self.dataset.components.is_empty() => {
                return Err(
                    "只有 multi-barframe 或多市场 Bar/DepthFrame 可声明 dataset.components".into(),
                );
            }
            _ => {}
        }
        Ok(())
    }

    /// 交叉一致性：合成数据指纹 / 配置摘要 / 策略版本 / 构建身份都必须与 `run` 块逐字相等。
    fn check_cross_references(&self) -> Result<(), String> {
        if let Some(analysis) = &self.report_analysis {
            let document = serde_json::from_str::<serde_json::Value>(&analysis.analysis).ok();
            let calendar_path = document
                .as_ref()
                .and_then(|value| value.pointer("/calendar/path"))
                .and_then(serde_json::Value::as_str);
            let calendar_sha256 = document
                .as_ref()
                .and_then(|value| value.pointer("/calendar/sha256"))
                .and_then(serde_json::Value::as_str);
            if analysis.kind != "benchmark"
                || analysis.input_path.trim().is_empty()
                || analysis.input_sha256.trim().is_empty()
                || analysis.input_fingerprint.trim().is_empty()
                || analysis.instrument.trim().is_empty()
                || analysis
                    .calendar_input_path
                    .as_ref()
                    .is_some_and(|path| path.trim().is_empty())
                || analysis
                    .calendar_input_sha256
                    .as_ref()
                    .is_some_and(|digest| digest.trim().is_empty())
                || analysis.calendar_input_path.is_some()
                    != analysis.calendar_input_sha256.is_some()
                || document.as_ref().is_none_or(|value| !value.is_object())
                || analysis.calendar_input_path.is_some() != calendar_path.is_some()
                || analysis.calendar_input_sha256.as_deref() != calendar_sha256
            {
                return Err(
                    "运行证据包 report_analysis 必须包含 benchmark 类型、输入身份和分析对象".into(),
                );
            }
        }
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

impl RunEvidencePayload {
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.check_run()?;
        self.check_artifacts()?;
        self.check_dataset()?;
        self.check_cross_references()?;
        self.check_assumptions()?;
        crate::run_evidence_fidelity::validate(self.fidelity.as_ref())?;
        self.check_evidence_honesty()?;
        self.check_recompute()?;
        if self.operator.trim().is_empty() {
            return Err("运行证据包必须写明 operator".into());
        }
        Ok(())
    }
}
