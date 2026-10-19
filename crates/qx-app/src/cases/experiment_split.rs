//! 实验的**数据切分**与**基准对照**：把方案 §8「参数实验」点名的 OOS / 走查与基准列
//! 从**纸面声明**变成**真的被算出来的数**（V13 R28）。
//!
//! 立案：`qx_spec::SplitPlan` 有完整的字段与内部一致性校验、`qx_spec::EXPERIMENT_BASELINES`
//! 被 `ExperimentSpec::validate` 逐值校验——但三个实验执行者**一次都没有引用** split / walk /
//! train / test，也没有任何地方算出 buy-and-hold 或 zero-signal 的收益。于是「这轮回测赚了
//! 113 bps」既没有样本外对照、也没有参照物：**schema 说了，执行者没做**。
//!
//! ## 口径
//!
//! * 切分以 **Bar 序号**计，不是时间戳——序号是唯一与数据集无关的坐标系，两条腿与批量任务
//!   都按它对齐。
//! * **选参只读训练窗**的样本内收益，报告只写测试窗的样本外收益。这是走查与"整段跑一遍再
//!   切片"的分界：后者会把测试窗的信息泄漏进选参。
//! * 权益序列从**产物读回**（`<run_id>.equity.csv`），不是从内存里的报告拿——与 `verify_run`
//!   同一条纪律：落盘的那份才是被复核的对象。
//! * 基准的 `buy-and-hold` 用**收盘价序列**算（策略的权益曲线不是买入持有），`zero-signal`
//!   是恒等式：不交易 ⇒ 收益 0、回撤 0。`none` 不产出任何列。

use super::artifacts::{equity_series, read_text};
use super::run_experiment::ExperimentCandidateResult;
use crate::error::{AppError, AppErrorCategory};
use qx_guanxing::Bar;
pub use qx_spec::{
    SplitPlan as ExperimentSplitPlan, EXPERIMENT_BASELINES as EXPERIMENT_BASELINE_NAMES,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 定点换算：基点 = 万分之一。
const BPS_SCALE: i128 = 10_000;
type RankedTrainingCandidate = (usize, Option<String>, BTreeMap<String, i128>, i32);
/// 防止极小步长和巨大数据集制造无界报告、耗尽内存或把取消延迟到分折结束。
pub const MAX_EXPERIMENT_FOLDS: usize = 10_000;

/// Split samples by Bar ordinal; only the execution-specific fold algorithm lives in qx-app.
pub(super) fn experiment_folds(
    plan: &ExperimentSplitPlan,
    total_bars: usize,
) -> Result<Vec<[usize; 4]>, AppError> {
    let train = usize::try_from(plan.train_bars)
        .map_err(|_| invalid("实验训练窗超过当前平台可表示的 Bar 数量"))?;
    let test = usize::try_from(plan.test_bars)
        .map_err(|_| invalid("实验测试窗超过当前平台可表示的 Bar 数量"))?;
    let span = train
        .checked_add(test)
        .ok_or_else(|| invalid("实验训练窗与测试窗长度溢出"))?;
    if total_bars < span {
        return Err(AppError::new(
                AppErrorCategory::FidelityInsufficient,
                format!(
                    "实验切分计划需要至少 {span} 根 Bar（训练 {train} + 测试 {test}），实际 {total_bars} 根"
                ),
            ));
    }
    let mut folds = Vec::new();
    if plan.walk_forward {
        let step = usize::try_from(plan.step_bars)
            .map_err(|_| invalid("实验走查步长超过当前平台可表示的 Bar 数量"))?;
        let last_start = total_bars - span;
        let mut start = 0usize;
        while start <= last_start {
            let train_end = start + train; // `start <= total_bars - span`，以下加法已界内。
            let test_end = train_end + test;
            folds.push([start, train_end, train_end, test_end]);
            if folds.len() > MAX_EXPERIMENT_FOLDS {
                return Err(invalid(format!(
                    "实验走查折数超过上限 {MAX_EXPERIMENT_FOLDS}；增大 step_bars 或缩小样本区间"
                )));
            }
            // `checked_add` 保证极端步长不会回绕成负向/零步进，死循环。
            let Some(next) = start.checked_add(step) else {
                break;
            };
            start = next;
        }
    } else {
        // 单段切分**只有一段**。第一版在这里也用循环推步长，于是 60 根样本会多切出
        // 第二段 `[30,50)+[50,60)`——"不打开走查"却滚动了两次，正是这条用例咬出来的。
        folds.push([0, train, train, train + test]);
    }
    if folds.is_empty() {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            format!("实验切分计划在 {total_bars} 根 Bar 上切不出任何一段完整的训练/测试窗"),
        ));
    }
    Ok(folds)
}

/// 一段走查窗口的裁决：选中的是哪个候选、样本内多少、样本外多少。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentFoldReport {
    pub fold: usize,
    /// 训练窗 `[start, end)`，按 Bar 序号。
    pub train_range: [u64; 2],
    /// 测试窗 `[start, end)`，按 Bar 序号。
    pub test_range: [u64; 2],
    /// 在训练窗上胜出的候选序号（`ExperimentCandidateResult.ordinal`）。
    pub selected_ordinal: usize,
    /// Strategy selected from the in-sample window; omitted for legacy single-strategy runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_strategy_kind: Option<String>,
    pub selected_parameters: BTreeMap<String, i128>,
    pub in_sample_return_bps: i32,
    pub out_of_sample_return_bps: i32,
}

/// 样本外评估的汇总。`folds` 只有一段时就是一次普通的训练/测试切分。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentOutOfSampleReport {
    pub folds: Vec<ExperimentFoldReport>,
    /// 各折训练窗收益的算术平均。
    pub in_sample_return_bps: i32,
    /// 各折测试窗收益的算术平均——这才是"样本外"那个数。
    pub out_of_sample_return_bps: i32,
    /// 样本内减样本外：正数表示训练窗上的优势没有延续到测试窗。
    pub degradation_bps: i32,
}

/// 一条基准对照列。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentBaselineReport {
    pub name: String,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
}

/// 算基准对照列。`none` 不产出行（它的意思正是"不要基准"）。
pub(super) fn evaluate_baselines(
    bars: &[Bar],
    requested: &[String],
) -> Vec<ExperimentBaselineReport> {
    let mut out = Vec::new();
    for name in requested {
        match name.as_str() {
            "buy-and-hold" => {
                let closes: Vec<i128> = bars.iter().map(|bar| bar.close).collect();
                out.push(ExperimentBaselineReport {
                    name: name.clone(),
                    return_bps: window_return_bps(&closes, 0, closes.len()),
                    max_drawdown_bps: window_max_drawdown_bps(&closes, 0, closes.len()),
                });
            }
            "zero-signal" => out.push(ExperimentBaselineReport {
                name: name.clone(),
                return_bps: 0,
                max_drawdown_bps: 0,
            }),
            _ => {}
        }
    }
    out
}

/// 按切分计划做样本外评估。每个候选的权益序列从它自己的 `equity.csv` 读回。
pub(super) fn evaluate_out_of_sample(
    plan: &ExperimentSplitPlan,
    total_bars: usize,
    candidates: &[ExperimentCandidateResult],
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<Option<ExperimentOutOfSampleReport>, AppError> {
    plan.validate().map_err(invalid)?;
    let folds = experiment_folds(plan, total_bars)?;
    // 权益序列逐候选读一次，折叠之间复用——折叠数是 O(样本/步长)，逐折重读是平方级放大。
    let mut series: Vec<(usize, Vec<i128>)> = Vec::new();
    for candidate in candidates {
        if is_cancelled() {
            return Ok(None);
        }
        let Some(outcome) = candidate.outcome.as_ref() else {
            continue;
        };
        let payload = read_text(&outcome.artifacts.equity)?;
        let values = equity_series(&payload).ok_or_else(|| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("权益曲线产物无法解析: {}", outcome.artifacts.equity),
            )
        })?;
        if values.len() != total_bars {
            return Err(AppError::new(
                AppErrorCategory::InternalInvariant,
                format!(
                    "候选 {} 的权益点数 {} 与样本根数 {total_bars} 不一致——切分按 Bar 序号对齐，\
                     点数不等就没有共同的坐标系",
                    candidate.ordinal,
                    values.len()
                ),
            ));
        }
        if values.iter().any(|value| *value <= 0) {
            return Err(AppError::new(
                AppErrorCategory::InternalInvariant,
                format!(
                    "候选 {} 的权益曲线包含非正权益，无法定义收益率基准",
                    candidate.ordinal
                ),
            ));
        }
        series.push((candidate.ordinal, values));
    }
    if series.is_empty() {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            "样本外评估至少要有一个成功候选的权益曲线",
        ));
    }

    let mut reports = Vec::with_capacity(folds.len());
    for (index, [train_start, train_end, test_start, test_end]) in folds.into_iter().enumerate() {
        if is_cancelled() {
            return Ok(None);
        }
        // 选参只看训练窗。并列时取序号最小的候选——排序必须确定，否则同一份 spec
        // 在两次运行里会选出不同的参数，OOS 分数也就不可复算。
        let mut best: Option<RankedTrainingCandidate> = None;
        for candidate in candidates {
            let Some((_, values)) = series
                .iter()
                .find(|(ordinal, _)| *ordinal == candidate.ordinal)
            else {
                continue;
            };
            let score = window_return_bps(values, train_start, train_end);
            let better = match &best {
                None => true,
                Some((ordinal, _, _, incumbent)) => {
                    score > *incumbent || (score == *incumbent && candidate.ordinal < *ordinal)
                }
            };
            if better {
                best = Some((
                    candidate.ordinal,
                    candidate.strategy_kind.clone(),
                    candidate.parameters.clone(),
                    score,
                ));
            }
        }
        let Some((selected_ordinal, selected_strategy_kind, selected_parameters, in_sample)) = best
        else {
            continue;
        };
        let values = series
            .iter()
            .find(|(ordinal, _)| *ordinal == selected_ordinal)
            .map(|(_, values)| values)
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!("选中的候选 {selected_ordinal} 没有权益序列"),
                )
            })?;
        reports.push(ExperimentFoldReport {
            fold: index + 1,
            train_range: [train_start as u64, train_end as u64],
            test_range: [test_start as u64, test_end as u64],
            selected_ordinal,
            selected_strategy_kind,
            selected_parameters,
            in_sample_return_bps: in_sample,
            out_of_sample_return_bps: window_return_bps(values, test_start, test_end),
        });
    }
    if reports.is_empty() {
        return Err(AppError::new(
            AppErrorCategory::FidelityInsufficient,
            "样本外评估没有任何一段窗口选出候选——没有成功候选就没有 OOS 分数",
        ));
    }
    let in_sample = mean_bps(reports.iter().map(|fold| fold.in_sample_return_bps));
    let out_of_sample = mean_bps(reports.iter().map(|fold| fold.out_of_sample_return_bps));
    Ok(Some(ExperimentOutOfSampleReport {
        folds: reports,
        in_sample_return_bps: in_sample,
        out_of_sample_return_bps: out_of_sample,
        degradation_bps: clamp_bps(in_sample as i128 - out_of_sample as i128),
    }))
}

/// 窗口 `[start, end)` 上的收益（基点），以窗口首点为基准。
fn window_return_bps(series: &[i128], start: usize, end: usize) -> i32 {
    if end <= start + 1 || end > series.len() {
        return 0;
    }
    let first = series[start];
    if first <= 0 {
        return 0;
    }
    let Some(delta) = series[end - 1].checked_sub(first) else {
        return if series[end - 1] > first {
            i32::MAX
        } else {
            i32::MIN
        };
    };
    let Some(scaled) = delta.checked_mul(BPS_SCALE) else {
        return if delta > 0 { i32::MAX } else { i32::MIN };
    };
    clamp_bps(scaled / first)
}

/// 窗口 `[start, end)` 上的最大回撤（基点）。
fn window_max_drawdown_bps(series: &[i128], start: usize, end: usize) -> u32 {
    if end <= start || end > series.len() {
        return 0;
    }
    let mut peak = series[start];
    let mut worst = 0i128;
    for &value in &series[start..end] {
        if value > peak {
            peak = value;
        }
        if peak > 0 {
            // Clamp on overflow rather than wrapping a catastrophic drawdown into a gain.
            let drawdown = peak
                .checked_sub(value)
                .and_then(|delta| delta.checked_mul(BPS_SCALE))
                .map(|scaled| scaled / peak)
                .unwrap_or(i128::MAX);
            worst = worst.max(drawdown);
        }
    }
    worst.clamp(0, u32::MAX as i128) as u32
}

/// 基点求和后取平均。用 `i128` 累加：候选多、窗口长时 `i32` 会静默回绕。
fn mean_bps(values: impl Iterator<Item = i32>) -> i32 {
    let mut total = 0i128;
    let mut count = 0i128;
    for value in values {
        total += value as i128;
        count += 1;
    }
    if count == 0 {
        return 0;
    }
    clamp_bps(total / count)
}

/// `i128` 基点收进 `i32`。溢出是"这个数已经不像收益率了"，饱和比回绕安全。
fn clamp_bps(value: i128) -> i32 {
    value.clamp(i32::MIN as i128, i32::MAX as i128) as i32
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_lengths_reject_integer_overflow() {
        let plan = ExperimentSplitPlan {
            train_bars: u64::MAX,
            test_bars: 1,
            walk_forward: false,
            step_bars: 0,
        };
        let error = experiment_folds(&plan, usize::MAX).expect_err("窗长加法必须拒绝溢出");
        assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    }

    #[test]
    fn walk_forward_fold_count_is_bounded() {
        let plan = ExperimentSplitPlan {
            train_bars: 1,
            test_bars: 1,
            walk_forward: true,
            step_bars: 1,
        };
        let error =
            experiment_folds(&plan, MAX_EXPERIMENT_FOLDS + 2).expect_err("不得生成无界 OOS 报告");
        assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    }

    #[test]
    fn extreme_returns_and_drawdowns_saturate_without_wrapping() {
        assert_eq!(window_return_bps(&[1, i128::MAX], 0, 2), i32::MAX);
        assert_eq!(window_return_bps(&[i128::MAX, i128::MIN], 0, 2), i32::MIN);
        assert_eq!(
            window_max_drawdown_bps(&[i128::MAX, i128::MIN], 0, 2),
            u32::MAX
        );
    }
}
