//! 运行记录（RunRecord）：一次运行及其产物的可复核身份。
//!
//! 规划口径（docs/qianxing-项目结构与GitHub竞品对比及优化方案-2026-10-06.md §6.2 / §7 P2）：
//! `RunRecord` 记录一次运行及其产物，**不依赖目录扫描猜最新结果**。这里把「完成」定义成
//! 硬约束：状态为 Completed 就必须有结束时间、重放裁决必须是 Verified、且至少引用一份产物，
//! 把「报告说完成、其实没复核」这种口径挡在对象层。

use crate::{digest_json, CapabilityLevel, FoundationDocument, FoundationKind};
use serde::{Deserialize, Serialize};

pub const RUN_RECORD_SCHEMA_VERSION: u32 = 1;

/// 一次运行的生命周期状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Pending => "pending",
            RunStatus::Running => "running",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
        }
    }
}

/// 重放裁决：`Verified` 才代表事件流可复核。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayVerdict {
    Verified,
    Mismatch,
    NotRun,
}

impl ReplayVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReplayVerdict::Verified => "verified",
            ReplayVerdict::Mismatch => "mismatch",
            ReplayVerdict::NotRun => "not_run",
        }
    }
}

/// 一份运行产物：名称、路径与内容摘要。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    pub name: String,
    pub path: String,
    pub digest: String,
}

/// 一次运行及其产物的可复核身份。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    pub schema_version: u32,
    pub run_id: String,
    pub status: RunStatus,
    pub input_digest: String,
    pub code_identity: String,
    pub config_fingerprint: String,
    pub started_at_ms: u64,
    #[serde(default)]
    pub finished_at_ms: Option<u64>,
    #[serde(default)]
    pub artifact_refs: Vec<ArtifactRef>,
    pub replay_verdict: ReplayVerdict,
    pub capability_level: CapabilityLevel,
}

impl RunRecord {
    /// 状态是否已经到达终态（终态之后不再接受新的产物引用）。
    pub fn is_terminal(&self) -> bool {
        matches!(self.status, RunStatus::Completed | RunStatus::Failed)
    }
}

impl FoundationDocument for RunRecord {
    const KIND: FoundationKind = FoundationKind::RunRecord;
    const SCHEMA_VERSION: u32 = RUN_RECORD_SCHEMA_VERSION;

    fn from_json(payload: &str) -> Result<Self, String> {
        let document: Self =
            serde_json::from_str(payload).map_err(|error| format!("运行记录解析失败: {error}"))?;
        document.validate()?;
        Ok(document)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != RUN_RECORD_SCHEMA_VERSION {
            return Err(format!(
                "运行记录 schema_version={} 不受支持（本构建只认 {}）",
                self.schema_version, RUN_RECORD_SCHEMA_VERSION
            ));
        }
        if self.run_id.trim().is_empty() {
            return Err("运行记录 run_id 不能为空".into());
        }
        if self.input_digest.trim().is_empty()
            || self.code_identity.trim().is_empty()
            || self.config_fingerprint.trim().is_empty()
        {
            return Err(
                "运行记录必须写明 input_digest / code_identity / config_fingerprint".into(),
            );
        }
        for artifact in &self.artifact_refs {
            if artifact.name.trim().is_empty()
                || artifact.path.trim().is_empty()
                || artifact.digest.trim().is_empty()
            {
                return Err("运行记录 artifact_refs 每项都要有 name / path / digest".into());
            }
        }
        if self.is_terminal() {
            let finished = self
                .finished_at_ms
                .ok_or_else(|| "运行记录已到终态但没有 finished_at_ms".to_string())?;
            if finished < self.started_at_ms {
                return Err("运行记录 finished_at_ms 早于 started_at_ms".into());
            }
        }
        if self.status == RunStatus::Completed && self.replay_verdict != ReplayVerdict::Verified {
            return Err(format!(
                "运行记录 status=completed 但 replay_verdict={}：未通过重放不得记为完成",
                self.replay_verdict.as_str()
            ));
        }
        if self.replay_verdict == ReplayVerdict::Verified && self.artifact_refs.is_empty() {
            return Err("运行记录 replay_verdict=verified 但没有可复核产物".into());
        }
        Ok(())
    }

    fn identity(&self) -> String {
        format!("{} [{}]", self.run_id, self.status.as_str())
    }

    fn fingerprint(&self) -> Result<String, String> {
        digest_json(self)
    }

    fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|error| format!("运行记录规范化失败: {error}"))
    }
}
