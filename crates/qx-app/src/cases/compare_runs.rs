//! Deterministic comparison for parameterized Bar backtest runs.

use super::attach;
use crate::{AppError, AppErrorCategory, RunContext};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const COMPARE_RUNS_SCHEMA_VERSION: u32 = 1;
pub const MAX_COMPARE_RUNS: usize = 10_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompareRunsSpec {
    pub schema_version: u32,
    pub runs: Vec<ComparedRun>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComparedRun {
    pub run_id: String,
    pub instrument: String,
    pub data_fingerprint: String,
    pub result_hash: String,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparedRunResult {
    pub rank: usize,
    pub run: ComparedRun,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareRunsResult {
    pub schema_version: u32,
    pub instrument: String,
    pub data_fingerprint: String,
    pub runs: Vec<ComparedRunResult>,
}

impl CompareRunsSpec {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("CompareRunsSpec JSON 无效: {error}"),
            )
        })
    }
}

impl CompareRunsResult {
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("CompareRunsResult 序列化失败: {error}"),
            )
        })
    }
}

/// Compare completed runs only when they share the exact instrument and data fingerprint.
/// Ranking is stable: return descending, drawdown ascending, then run id ascending.
pub fn compare_runs(
    spec: &CompareRunsSpec,
    context: &RunContext,
) -> Result<CompareRunsResult, AppError> {
    let result = (|| {
        if spec.schema_version != COMPARE_RUNS_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "CompareRunsSpec schema_version={} 本版本只认 {}",
                    spec.schema_version, COMPARE_RUNS_SCHEMA_VERSION
                ),
            ));
        }
        if spec.runs.len() < 2 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "参数对比至少需要两次已完成的回测",
            ));
        }
        if spec.runs.len() > MAX_COMPARE_RUNS {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!("参数对比最多接受 {MAX_COMPARE_RUNS} 次回测"),
            ));
        }
        let first = &spec.runs[0];
        let mut ids = BTreeSet::new();
        for run in &spec.runs {
            if run.run_id.trim().is_empty()
                || run.instrument.trim().is_empty()
                || run.data_fingerprint.trim().is_empty()
                || run.result_hash.trim().is_empty()
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "对比输入包含空的 run_id/instrument/data_fingerprint/result_hash",
                ));
            }
            if !ids.insert(&run.run_id) {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("run_id 重复: {}", run.run_id),
                ));
            }
            if run.instrument != first.instrument || run.data_fingerprint != first.data_fingerprint
            {
                return Err(AppError::new(
                    AppErrorCategory::Conflict,
                    "对比回测必须使用相同市场标的与数据指纹",
                ));
            }
            if run.max_drawdown_bps > 10_000 {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("run {} 的 max_drawdown_bps 超出 0..=10000", run.run_id),
                ));
            }
        }
        let mut runs = spec.runs.clone();
        runs.sort_by(|a, b| {
            b.return_bps
                .cmp(&a.return_bps)
                .then_with(|| a.max_drawdown_bps.cmp(&b.max_drawdown_bps))
                .then_with(|| a.run_id.cmp(&b.run_id))
        });
        Ok(CompareRunsResult {
            schema_version: COMPARE_RUNS_SCHEMA_VERSION,
            instrument: first.instrument.clone(),
            data_fingerprint: first.data_fingerprint.clone(),
            runs: runs
                .into_iter()
                .enumerate()
                .map(|(index, run)| ComparedRunResult {
                    rank: index + 1,
                    run,
                })
                .collect(),
        })
    })();
    result.map_err(|error| attach(error, context.correlation_id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallerCapability, RunContext};

    fn run(id: &str, return_bps: i32, drawdown: u32) -> ComparedRun {
        ComparedRun {
            run_id: id.into(),
            instrument: "BTC-USDT".into(),
            data_fingerprint: "bars:abc".into(),
            result_hash: format!("hash-{id}"),
            return_bps,
            max_drawdown_bps: drawdown,
        }
    }

    fn ctx() -> RunContext {
        RunContext::new(CallerCapability::Research, "compare-case")
    }

    #[test]
    fn ranks_by_return_then_drawdown_then_identity_deterministically() {
        let spec = CompareRunsSpec {
            schema_version: COMPARE_RUNS_SCHEMA_VERSION,
            runs: vec![run("z", 100, 500), run("b", 100, 300), run("a", 200, 900)],
        };
        let output = compare_runs(&spec, &ctx()).unwrap();
        assert_eq!(
            output
                .runs
                .iter()
                .map(|item| item.run.run_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "z"]
        );
        assert_eq!(
            output.runs.iter().map(|item| item.rank).collect::<Vec<_>>(),
            [1, 2, 3]
        );
    }

    #[test]
    fn refuses_mixed_market_data_and_duplicate_run_ids() {
        let mut spec = CompareRunsSpec {
            schema_version: 1,
            runs: vec![run("a", 1, 1), run("b", 2, 2)],
        };
        spec.runs[1].data_fingerprint = "bars:other".into();
        assert_eq!(
            compare_runs(&spec, &ctx()).unwrap_err().category(),
            AppErrorCategory::Conflict
        );
        spec.runs[1] = run("a", 2, 2);
        assert_eq!(
            compare_runs(&spec, &ctx()).unwrap_err().category(),
            AppErrorCategory::InvalidInput
        );
    }

    #[test]
    fn rejects_invalid_drawdown_and_short_comparison_sets() {
        let mut spec = CompareRunsSpec {
            schema_version: 1,
            runs: vec![run("a", 1, 1), run("b", 2, 2)],
        };
        spec.runs[0].max_drawdown_bps = 10_001;
        assert_eq!(
            compare_runs(&spec, &ctx()).unwrap_err().category(),
            AppErrorCategory::InvalidInput
        );
        spec.runs.truncate(1);
        assert_eq!(
            compare_runs(&spec, &ctx()).unwrap_err().category(),
            AppErrorCategory::InvalidInput
        );
    }
}
