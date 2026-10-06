//! 实验规格（ExperimentSpec）：参数、数据切分、成本与基线的唯一声明。
//!
//! 规划口径（docs/qianxing-项目结构与GitHub竞品对比及优化方案-2026-10-06.md §6.2 / §7 P2）：
//! 相同 `ExperimentSpec + code_identity + dataset fingerprint` 必须得到相同参数排序与结果哈希。
//! 因此这里把随机种子、训练/测试切分、成本与风控规则集、模型版本全部写进同一份身份文档，
//! 而不是散落在命令行的可选参数里。

use crate::{digest_json, FoundationDocument, FoundationKind};
use serde::{Deserialize, Serialize};

pub const EXPERIMENT_SPEC_SCHEMA_VERSION: u32 = 1;

/// 规划 §7 P2 的策略基线/反事实对照取值域。
pub const EXPERIMENT_BASELINES: [&str; 3] = ["buy-and-hold", "zero-signal", "none"];

/// 一个参数维度的取值空间。取值以字符串承载，由实验执行器按策略声明的类型解析。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParameterSpace {
    pub name: String,
    pub values: Vec<String>,
}

/// 训练/测试切分与 walk-forward 计划。`step_bars` 只在 walk-forward 打开时有意义。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplitPlan {
    pub train_bars: u64,
    pub test_bars: u64,
    pub walk_forward: bool,
    pub step_bars: u64,
}

/// 实验规格：一次可复跑实验的全部输入身份。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentSpec {
    pub schema_version: u32,
    pub experiment_id: String,
    pub strategy_ref: String,
    pub dataset_id: String,
    pub seed: u64,
    pub initial_capital_raw: i128,
    pub parameter_space: Vec<ParameterSpace>,
    pub split_plan: SplitPlan,
    pub baselines: Vec<String>,
    pub cost_model_ref: String,
    pub risk_rule_set: String,
    pub model_version: String,
}

impl FoundationDocument for ExperimentSpec {
    const KIND: FoundationKind = FoundationKind::Experiment;
    const SCHEMA_VERSION: u32 = EXPERIMENT_SPEC_SCHEMA_VERSION;

    fn from_json(payload: &str) -> Result<Self, String> {
        let document: Self =
            serde_json::from_str(payload).map_err(|error| format!("实验规格解析失败: {error}"))?;
        document.validate()?;
        Ok(document)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != EXPERIMENT_SPEC_SCHEMA_VERSION {
            return Err(format!(
                "实验规格 schema_version={} 不受支持（本构建只认 {}）",
                self.schema_version, EXPERIMENT_SPEC_SCHEMA_VERSION
            ));
        }
        if self.experiment_id.trim().is_empty() || self.strategy_ref.trim().is_empty() {
            return Err("实验规格 experiment_id 与 strategy_ref 都是必需项".into());
        }
        if self.dataset_id.trim().is_empty() {
            return Err("实验规格 dataset_id 是必需项".into());
        }
        if self.initial_capital_raw <= 0 {
            return Err("实验规格 initial_capital_raw 必须为正（单位 raw）".into());
        }
        if self.parameter_space.is_empty() {
            return Err("实验规格至少要声明一个参数维度".into());
        }
        for space in &self.parameter_space {
            if space.name.trim().is_empty() || space.values.is_empty() {
                return Err("实验规格参数维度要有 name 与非空 values".into());
            }
            let mut seen = std::collections::BTreeSet::new();
            for value in &space.values {
                if !seen.insert(value.as_str()) {
                    return Err(format!("实验规格参数 {} 的取值有重复：{value}", space.name));
                }
            }
        }
        let split = &self.split_plan;
        if split.train_bars == 0 || split.test_bars == 0 {
            return Err("实验规格 split_plan 的 train_bars 与 test_bars 都必须为正".into());
        }
        if split.walk_forward && split.step_bars == 0 {
            return Err("实验规格打开 walk_forward 时 step_bars 必须为正".into());
        }
        if !split.walk_forward && split.step_bars != 0 {
            return Err("实验规格未打开 walk_forward 时 step_bars 必须为 0".into());
        }
        if self.baselines.is_empty() {
            return Err("实验规格至少要声明一个基线（可用 none）".into());
        }
        for baseline in &self.baselines {
            if !EXPERIMENT_BASELINES.contains(&baseline.as_str()) {
                return Err(format!(
                    "实验规格基线 {baseline} 不在 {} 内",
                    EXPERIMENT_BASELINES.join(" / ")
                ));
            }
        }
        if self.cost_model_ref.trim().is_empty()
            || self.risk_rule_set.trim().is_empty()
            || self.model_version.trim().is_empty()
        {
            return Err(
                "实验规格必须写明 cost_model_ref / risk_rule_set / model_version（无模型写 none）"
                    .into(),
            );
        }
        Ok(())
    }

    fn identity(&self) -> String {
        format!("{}@seed{}", self.experiment_id, self.seed)
    }

    fn fingerprint(&self) -> Result<String, String> {
        digest_json(self)
    }

    fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|error| format!("实验规格规范化失败: {error}"))
    }
}
