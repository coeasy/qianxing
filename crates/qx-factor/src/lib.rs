//! 因子与特征研究边界。
//!
//! 这里保存的是可追溯的研究工件，不直接修改 Kernel、账簿或订单状态。
//! 特征定义、物化结果和候选策略分开建模，避免“一个 DataFrame 既是输入又是结论”。

use qx_core::SCALE;
use qx_core::{Fnv1a, InstrumentId, RunManifest};
use qx_guanxing::{CandidateConfig, DataView, ParameterSet};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

mod execution_plan;

pub use execution_plan::{
    FactorExecutionPlan, FactorIncrementalProvenance, FactorPlanNode,
    FACTOR_EXECUTION_PLAN_SCHEMA_VERSION,
};

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FeatureDefinition {
    pub name: String,
    pub version: String,
    pub formula: String,
    pub input_fields: Vec<String>,
    pub dependencies: Vec<String>,
    pub point_in_time: bool,
}

impl FeatureDefinition {
    pub fn validate(&self) -> Result<(), FactorError> {
        if self.name.trim().is_empty() || self.version.trim().is_empty() {
            return Err(FactorError::Invalid("feature name/version 不能为空".into()));
        }
        if self.formula.trim().is_empty() || self.input_fields.is_empty() {
            return Err(FactorError::Invalid(
                "feature 必须声明公式和输入字段".into(),
            ));
        }
        if !self.point_in_time {
            return Err(FactorError::NonPointInTime(self.name.clone()));
        }
        if self
            .dependencies
            .iter()
            .any(|dependency| dependency == &self.key())
        {
            return Err(FactorError::Cycle(self.key()));
        }
        Ok(())
    }

    pub fn key(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FeatureArtifact {
    pub feature_key: String,
    pub input_fingerprint: String,
    pub as_of: u64,
    pub coverage_bps: u32,
    pub values: BTreeMap<InstrumentId, i128>,
}

/// 由已通过质量门的 PIT DataView 计算一个可审计的滚动收益因子。
/// 返回值仍使用定点 raw：`(latest / lookback - 1) * SCALE`。
pub fn compute_momentum(
    feature: &FeatureDefinition,
    instrument: InstrumentId,
    view: &DataView,
    as_of: u64,
    lookback: usize,
    input_fingerprint: &str,
) -> Result<FeatureArtifact, FactorError> {
    feature.validate()?;
    if lookback == 0 || input_fingerprint.trim().is_empty() {
        return Err(FactorError::Invalid(
            "lookback 和 input_fingerprint 不能为空".into(),
        ));
    }
    let visible = view.as_of(as_of);
    if visible.len() <= lookback {
        return Err(FactorError::Invalid(
            "PIT 数据不足以计算 lookback 因子".into(),
        ));
    }
    let latest = visible[visible.len() - 1].close;
    let previous = visible[visible.len() - 1 - lookback].close;
    if previous <= 0 {
        return Err(FactorError::Invalid("因子基准价格必须为正".into()));
    }
    let value = latest
        .checked_mul(SCALE)
        .and_then(|v| v.checked_div(previous))
        .and_then(|v| v.checked_sub(SCALE))
        .ok_or_else(|| FactorError::Invalid("因子计算溢出".into()))?;
    let artifact = FeatureArtifact {
        feature_key: feature.key(),
        input_fingerprint: input_fingerprint.into(),
        as_of,
        coverage_bps: 10_000,
        values: [(instrument, value)].into_iter().collect(),
    };
    artifact.validate()?;
    Ok(artifact)
}

pub fn apply_transform(
    values: &BTreeMap<InstrumentId, i128>,
    transform: &FactorTransform,
) -> Result<BTreeMap<InstrumentId, i128>, FactorError> {
    transform.validate()?;
    if values.is_empty() {
        return Ok(BTreeMap::new());
    }
    match transform {
        FactorTransform::Standardize => {
            let sum = values
                .values()
                .try_fold(0_i128, |sum, value| sum.checked_add(*value))
                .ok_or_else(|| FactorError::Invalid("standardize 求和溢出".into()))?;
            let mean = sum / values.len() as i128;
            Ok(values
                .iter()
                .map(|(instrument, value)| (instrument.clone(), value.saturating_sub(mean)))
                .collect())
        }
        FactorTransform::Winsorize {
            lower_bps,
            upper_bps,
        } => {
            let mut sorted = values.values().copied().collect::<Vec<_>>();
            sorted.sort_unstable();
            let index =
                |bps: u32| ((sorted.len().saturating_sub(1) as u64 * bps as u64) / 10_000) as usize;
            let low = sorted[index(*lower_bps)];
            let high = sorted[index(*upper_bps)];
            Ok(values
                .iter()
                .map(|(instrument, value)| (instrument.clone(), (*value).clamp(low, high)))
                .collect())
        }
        FactorTransform::Neutralize { .. } => Err(FactorError::Invalid(
            "neutralize 必须通过 apply_transform_with_groups 提供分组标签".into(),
        )),
    }
}

/// 按分组做组内去均值；缺少任何标的分组时拒绝，而不是静默退化为原值。
pub fn apply_transform_with_groups(
    values: &BTreeMap<InstrumentId, i128>,
    groups: &BTreeMap<InstrumentId, String>,
    transform: &FactorTransform,
) -> Result<BTreeMap<InstrumentId, i128>, FactorError> {
    transform.validate()?;
    let FactorTransform::Neutralize { .. } = transform else {
        return apply_transform(values, transform);
    };
    if values.keys().any(|instrument| {
        groups
            .get(instrument)
            .is_none_or(|group| group.trim().is_empty())
    }) {
        return Err(FactorError::Invalid("neutralize 缺少分组标签".into()));
    }
    let mut sums = BTreeMap::<&str, i128>::new();
    let mut counts = BTreeMap::<&str, i128>::new();
    for (instrument, value) in values {
        let group = groups[instrument].as_str();
        let sum = sums
            .get(group)
            .copied()
            .unwrap_or(0)
            .checked_add(*value)
            .ok_or_else(|| FactorError::Invalid("neutralize 分组求和溢出".into()))?;
        sums.insert(group, sum);
        counts.insert(group, counts.get(group).copied().unwrap_or(0) + 1);
    }
    values
        .iter()
        .map(|(instrument, value)| {
            let group = groups[instrument].as_str();
            let mean = sums[group] / counts[group];
            Ok((instrument.clone(), value.saturating_sub(mean)))
        })
        .collect()
}

impl FeatureArtifact {
    pub fn validate(&self) -> Result<(), FactorError> {
        if self.feature_key.trim().is_empty() || self.input_fingerprint.trim().is_empty() {
            return Err(FactorError::Invalid("feature artifact 缺少血缘指纹".into()));
        }
        if self.values.is_empty() {
            return Err(FactorError::Invalid(
                "feature artifact 不能没有观测值".into(),
            ));
        }
        if self.coverage_bps > 10_000 {
            return Err(FactorError::Invalid("coverage 必须在 0..=10000 bps".into()));
        }
        Ok(())
    }

    pub fn digest(&self) -> u64 {
        let mut h = Fnv1a::new();
        h.write_text(&self.feature_key);
        h.write_text(&self.input_fingerprint);
        h.write_u64(self.as_of);
        h.write_u64(self.coverage_bps as u64);
        for (instrument, value) in &self.values {
            h.write_text(&format!("{}", instrument));
            h.write_i128(*value);
        }
        h.finish()
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum FactorTransform {
    Standardize,
    Winsorize { lower_bps: u32, upper_bps: u32 },
    Neutralize { group: String },
}

impl FactorTransform {
    pub fn validate(&self) -> Result<(), FactorError> {
        match self {
            Self::Standardize => Ok(()),
            Self::Winsorize {
                lower_bps,
                upper_bps,
            } if *lower_bps < *upper_bps && *upper_bps <= 10_000 => Ok(()),
            Self::Winsorize { .. } => Err(FactorError::Invalid("winsorize 分位点非法".into())),
            Self::Neutralize { group } if !group.trim().is_empty() => Ok(()),
            Self::Neutralize { .. } => {
                Err(FactorError::Invalid("neutralize group 不能为空".into()))
            }
        }
    }
}

/// 因子分析的一个横截面观测。
///
/// `factor_value` 和 `forward_returns` 必须来自同一个 PIT 截止点；本类型只负责
/// 研究统计，不会把观测直接写入交易账簿。暴露值使用调用方约定的 bps 单位。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FactorObservation {
    pub timestamp: u64,
    pub instrument: InstrumentId,
    pub factor_value: Option<i128>,
    pub forward_returns: BTreeMap<u32, i128>,
    pub previous_exposure: Option<i128>,
    pub target_exposure: Option<i128>,
    pub capacity_raw: Option<i128>,
    pub exposures: BTreeMap<String, i32>,
    #[serde(default)]
    pub group_labels: BTreeMap<String, String>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FactorAnalysisConfig {
    pub primary_horizon: u32,
    pub decay_horizon: Option<u32>,
    pub input_fingerprint: String,
    pub missing_policy: String,
    pub transform: Option<FactorTransform>,
}

impl Default for FactorAnalysisConfig {
    fn default() -> Self {
        Self {
            primary_horizon: 1,
            decay_horizon: None,
            input_fingerprint: String::new(),
            missing_policy: default_missing_policy(),
            transform: None,
        }
    }
}

impl FactorAnalysisConfig {
    fn validate(&self) -> Result<(), FactorError> {
        if self.primary_horizon == 0 {
            return Err(FactorError::Invalid("primary_horizon 必须大于 0".into()));
        }
        if self.input_fingerprint.trim().is_empty() {
            return Err(FactorError::Invalid("input_fingerprint 不能为空".into()));
        }
        if self.decay_horizon == Some(0) {
            return Err(FactorError::Invalid("decay_horizon 必须大于 0".into()));
        }
        if !matches!(self.missing_policy.as_str(), "reject" | "skip" | "zero") {
            return Err(FactorError::Invalid(
                "missing_policy 仅支持 reject/skip/zero".into(),
            ));
        }
        if let Some(transform) = &self.transform {
            transform.validate()?;
        }
        Ok(())
    }
}

/// 从真实观测计算 FactorReport。
///
/// 统计均按 timestamp 分组后再做横截面平均，避免把不同交易时点直接混成一个
/// 样本。相关系数使用定点整数协方差和整数平方根，避免研究报告依赖平台浮点舍入。
pub fn analyze_factor(
    feature_key: &str,
    observations: &[FactorObservation],
    config: &FactorAnalysisConfig,
) -> Result<FactorReport, FactorError> {
    if feature_key.trim().is_empty() || observations.is_empty() {
        return Err(FactorError::Invalid("因子分析缺少 feature 或观测".into()));
    }
    config.validate()?;

    let mut by_time = BTreeMap::<u64, Vec<&FactorObservation>>::new();
    let mut seen = BTreeSet::new();
    for observation in observations {
        if !seen.insert((observation.timestamp, observation.instrument.clone())) {
            return Err(FactorError::Invalid("同一时点存在重复标的观测".into()));
        }
        by_time
            .entry(observation.timestamp)
            .or_default()
            .push(observation);
    }

    let mut primary_ic = Vec::new();
    let mut primary_rank_ic = Vec::new();
    let mut decay_ic = Vec::new();
    let mut valid_pairs = 0_u64;
    for rows in by_time.values() {
        let mut primary = Vec::new();
        let mut decay = Vec::new();
        for row in rows {
            let Some(factor) = resolve_missing(row.factor_value, &config.missing_policy)? else {
                continue;
            };
            let Some(target) = resolve_missing(
                row.forward_returns.get(&config.primary_horizon).copied(),
                &config.missing_policy,
            )?
            else {
                continue;
            };
            primary.push((row.instrument.clone(), factor, target));
            if let Some(horizon) = config.decay_horizon {
                if let Some(target) = resolve_missing(
                    row.forward_returns.get(&horizon).copied(),
                    &config.missing_policy,
                )? {
                    decay.push((row.instrument.clone(), factor, target));
                }
            }
        }
        if primary.len() >= 2 {
            if let Some(transform) = &config.transform {
                let values = primary
                    .iter()
                    .map(|(instrument, factor, _)| (instrument.clone(), *factor))
                    .collect::<BTreeMap<_, _>>();
                let transformed = match transform {
                    FactorTransform::Neutralize { group } => {
                        let groups = rows
                            .iter()
                            .filter(|row| values.contains_key(&row.instrument))
                            .map(|row| {
                                (
                                    row.instrument.clone(),
                                    row.group_labels.get(group).cloned().unwrap_or_default(),
                                )
                            })
                            .collect::<BTreeMap<_, _>>();
                        apply_transform_with_groups(&values, &groups, transform)?
                    }
                    _ => apply_transform(&values, transform)?,
                };
                for (instrument, factor, _) in &mut primary {
                    *factor = transformed[instrument];
                }
            }
            let xs = primary.iter().map(|(_, x, _)| *x).collect::<Vec<_>>();
            let ys = primary.iter().map(|(_, _, y)| *y).collect::<Vec<_>>();
            primary_ic.push(correlation_bps(&xs, &ys)?);
            let ranks = rank_values(&primary);
            let rank_x = ranks.iter().map(|(_, x, _)| *x).collect::<Vec<_>>();
            let rank_y = ranks.iter().map(|(_, _, y)| *y).collect::<Vec<_>>();
            primary_rank_ic.push(correlation_bps(&rank_x, &rank_y)?);
            valid_pairs = valid_pairs
                .checked_add(primary.len() as u64)
                .ok_or_else(|| FactorError::Invalid("因子样本数溢出".into()))?;
        }
        if decay.len() >= 2 {
            let xs = decay.iter().map(|(_, x, _)| *x).collect::<Vec<_>>();
            let ys = decay.iter().map(|(_, _, y)| *y).collect::<Vec<_>>();
            decay_ic.push(correlation_bps(&xs, &ys)?);
        }
    }
    if primary_ic.is_empty() {
        return Err(FactorError::Invalid(
            "每个时间横截面至少需要两个有效样本".into(),
        ));
    }

    if config.decay_horizon.is_some() && decay_ic.is_empty() {
        return Err(FactorError::Invalid("衰减 horizon 缺少有效横截面".into()));
    }
    let turnover_bps = turnover_bps(observations)?;
    let capacity_raw = observations
        .iter()
        .filter_map(|observation| observation.capacity_raw)
        .filter(|capacity| *capacity >= 0)
        .min()
        .unwrap_or(0);
    let exposures = average_exposures(observations)?;
    let ic_bps = average_i32(&primary_ic);
    let rank_ic_bps = average_i32(&primary_rank_ic);
    let decay_bps = config
        .decay_horizon
        .map(|_| ic_bps.saturating_sub(average_i32(&decay_ic)))
        .unwrap_or(0);
    let coverage_bps = ((valid_pairs as u128)
        .saturating_mul(10_000)
        .checked_div(observations.len() as u128)
        .unwrap_or(0)
        .min(10_000)) as u32;
    let observation_hash = observations_digest(observations);
    let analysis_start = observations
        .iter()
        .map(|row| row.timestamp)
        .min()
        .unwrap_or(0);
    let analysis_end = observations
        .iter()
        .map(|row| row.timestamp)
        .max()
        .unwrap_or(0);
    let report = FactorReport {
        feature_key: feature_key.into(),
        input_fingerprint: config.input_fingerprint.clone(),
        observation_hash,
        analysis_start,
        analysis_end,
        sample_count: valid_pairs,
        coverage_bps,
        ic_bps,
        rank_ic_bps,
        turnover_bps,
        transform: config.transform.clone(),
        missing_policy: config.missing_policy.clone(),
        decay_bps,
        capacity_raw,
        exposures,
    };
    report.validate()?;
    Ok(report)
}

fn observations_digest(observations: &[FactorObservation]) -> u64 {
    let mut rows = observations.to_vec();
    rows.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.instrument.cmp(&right.instrument))
    });
    let mut hash = Fnv1a::new();
    for row in rows {
        hash.write_u64(row.timestamp);
        hash.write_text(&row.instrument.to_string());
        match row.factor_value {
            Some(value) => {
                hash.write_u64(1);
                hash.write_i128(value);
            }
            None => hash.write_u64(0),
        }
        for value in [row.previous_exposure, row.target_exposure, row.capacity_raw] {
            match value {
                Some(value) => {
                    hash.write_u64(1);
                    hash.write_i128(value);
                }
                None => hash.write_u64(0),
            }
        }
        for (horizon, value) in row.forward_returns {
            hash.write_u64(horizon as u64);
            hash.write_i128(value);
        }
        for (name, value) in row.exposures {
            hash.write_text(&name);
            hash.write_i128(i128::from(value));
        }
        for (name, value) in row.group_labels {
            hash.write_text(&name);
            hash.write_text(&value);
        }
    }
    hash.finish()
}

fn resolve_missing(value: Option<i128>, policy: &str) -> Result<Option<i128>, FactorError> {
    match (value, policy) {
        (Some(value), _) => Ok(Some(value)),
        (None, "reject") => Err(FactorError::Invalid("因子观测存在缺失值".into())),
        (None, "skip") => Ok(None),
        (None, "zero") => Ok(Some(0)),
        (None, _) => Err(FactorError::Invalid("missing_policy 非法".into())),
    }
}

fn correlation_bps(xs: &[i128], ys: &[i128]) -> Result<i32, FactorError> {
    if xs.len() != ys.len() || xs.len() < 2 {
        return Err(FactorError::Invalid("相关性至少需要两个配对样本".into()));
    }
    let n = xs.len() as i128;
    let sum_x = xs
        .iter()
        .try_fold(0_i128, |sum, value| sum.checked_add(*value));
    let sum_y = ys
        .iter()
        .try_fold(0_i128, |sum, value| sum.checked_add(*value));
    let sum_xy = xs
        .iter()
        .zip(ys)
        .try_fold(0_i128, |sum, (x, y)| sum.checked_add(x.checked_mul(*y)?));
    let sum_x2 = xs.iter().try_fold(0_i128, |sum, value| {
        sum.checked_add(value.checked_mul(*value)?)
    });
    let sum_y2 = ys.iter().try_fold(0_i128, |sum, value| {
        sum.checked_add(value.checked_mul(*value)?)
    });
    let (Some(sum_x), Some(sum_y), Some(sum_xy), Some(sum_x2), Some(sum_y2)) =
        (sum_x, sum_y, sum_xy, sum_x2, sum_y2)
    else {
        return Err(FactorError::Invalid("相关性计算溢出".into()));
    };
    let covariance = n
        .checked_mul(sum_xy)
        .and_then(|value| value.checked_sub(sum_x.checked_mul(sum_y)?))
        .ok_or_else(|| FactorError::Invalid("相关性协方差溢出".into()))?;
    let variance_x = n
        .checked_mul(sum_x2)
        .and_then(|value| value.checked_sub(sum_x.checked_mul(sum_x)?))
        .ok_or_else(|| FactorError::Invalid("相关性方差溢出".into()))?;
    let variance_y = n
        .checked_mul(sum_y2)
        .and_then(|value| value.checked_sub(sum_y.checked_mul(sum_y)?))
        .ok_or_else(|| FactorError::Invalid("相关性方差溢出".into()))?;
    if variance_x <= 0 || variance_y <= 0 {
        return Ok(0);
    }
    let denominator = (variance_x as u128)
        .checked_mul(variance_y as u128)
        .ok_or_else(|| FactorError::Invalid("相关性分母溢出".into()))?;
    let denominator = integer_sqrt(denominator);
    if denominator == 0 {
        return Ok(0);
    }
    let scaled = covariance
        .checked_mul(10_000)
        .and_then(|value| value.checked_div(i128::try_from(denominator).ok()?))
        .ok_or_else(|| FactorError::Invalid("相关性缩放溢出".into()))?;
    Ok(scaled.clamp(-10_000, 10_000) as i32)
}

fn rank_values(values: &[(InstrumentId, i128, i128)]) -> Vec<(InstrumentId, i128, i128)> {
    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| {
        left.1
            .cmp(&right.1)
            .then_with(|| left.0.to_string().cmp(&right.0.to_string()))
    });
    let mut ranks = BTreeMap::new();
    let mut start = 0_usize;
    while start < sorted.len() {
        let mut end = start + 1;
        while end < sorted.len() && sorted[end].1 == sorted[start].1 {
            end += 1;
        }
        let rank_twice = (start as i128 + 1)
            .checked_add(end as i128)
            .unwrap_or(i128::MAX);
        for row in &sorted[start..end] {
            ranks.insert(row.0.clone(), rank_twice);
        }
        start = end;
    }
    sorted
        .into_iter()
        .map(|(instrument, _, target)| (instrument.clone(), ranks[&instrument], target))
        .collect()
}

fn integer_sqrt(value: u128) -> u128 {
    if value < 2 {
        return value;
    }
    let mut low = 1_u128;
    let mut high = 1_u128 << 64;
    while low + 1 < high {
        let mid = low + (high - low) / 2;
        if mid <= value / mid {
            low = mid;
        } else {
            high = mid;
        }
    }
    low
}

fn average_i32(values: &[i32]) -> i32 {
    if values.is_empty() {
        return 0;
    }
    (values.iter().map(|value| i64::from(*value)).sum::<i64>() / values.len() as i64)
        .clamp(-10_000, 10_000) as i32
}

fn turnover_bps(observations: &[FactorObservation]) -> Result<u32, FactorError> {
    let mut by_time = BTreeMap::<u64, (i128, i128)>::new();
    for observation in observations {
        let (Some(previous), Some(target)) =
            (observation.previous_exposure, observation.target_exposure)
        else {
            continue;
        };
        let delta = target
            .checked_sub(previous)
            .ok_or_else(|| FactorError::Invalid("换手计算溢出".into()))?
            .unsigned_abs();
        let target_abs = target.unsigned_abs();
        let (numerator, denominator) = by_time.entry(observation.timestamp).or_default();
        *numerator = numerator
            .checked_add(
                i128::try_from(delta).map_err(|_| FactorError::Invalid("换手溢出".into()))?,
            )
            .ok_or_else(|| FactorError::Invalid("换手计算溢出".into()))?;
        *denominator = denominator
            .checked_add(
                i128::try_from(target_abs).map_err(|_| FactorError::Invalid("换手溢出".into()))?,
            )
            .ok_or_else(|| FactorError::Invalid("换手计算溢出".into()))?;
    }
    let values = by_time
        .values()
        .filter(|(_, denominator)| *denominator > 0)
        .map(|(numerator, denominator)| {
            numerator
                .checked_mul(10_000)
                .and_then(|value| value.checked_div(*denominator))
                .ok_or_else(|| FactorError::Invalid("换手缩放溢出".into()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let sum = values
        .iter()
        .try_fold(0_i128, |sum, value| sum.checked_add(*value))
        .ok_or_else(|| FactorError::Invalid("换手平均值溢出".into()))?;
    Ok(sum
        .checked_div(values.len().max(1) as i128)
        .unwrap_or(0)
        .clamp(0, 10_000) as u32)
}

fn average_exposures(
    observations: &[FactorObservation],
) -> Result<BTreeMap<String, i32>, FactorError> {
    let mut sums = BTreeMap::<String, (i64, u64)>::new();
    for observation in observations {
        for (name, value) in &observation.exposures {
            let entry = sums.entry(name.clone()).or_default();
            entry.0 = entry
                .0
                .checked_add(i64::from(*value))
                .ok_or_else(|| FactorError::Invalid("暴露求和溢出".into()))?;
            entry.1 = entry
                .1
                .checked_add(1)
                .ok_or_else(|| FactorError::Invalid("暴露样本数溢出".into()))?;
        }
    }
    Ok(sums
        .into_iter()
        .map(|(name, (sum, count))| (name, (sum / count as i64).clamp(-10_000, 10_000) as i32))
        .collect())
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct FactorReport {
    pub feature_key: String,
    #[serde(default)]
    pub input_fingerprint: String,
    #[serde(default)]
    pub observation_hash: u64,
    #[serde(default)]
    pub analysis_start: u64,
    #[serde(default)]
    pub analysis_end: u64,
    pub sample_count: u64,
    pub coverage_bps: u32,
    pub ic_bps: i32,
    pub rank_ic_bps: i32,
    pub turnover_bps: u32,
    pub transform: Option<FactorTransform>,
    #[serde(default = "default_missing_policy")]
    pub missing_policy: String,
    #[serde(default)]
    pub decay_bps: i32,
    #[serde(default)]
    pub capacity_raw: i128,
    #[serde(default)]
    pub exposures: BTreeMap<String, i32>,
}

fn default_missing_policy() -> String {
    "reject".into()
}

impl FactorReport {
    pub fn validate(&self) -> Result<(), FactorError> {
        if self.feature_key.trim().is_empty()
            || self.input_fingerprint.trim().is_empty()
            || self.observation_hash == 0
            || self.analysis_start > self.analysis_end
            || self.coverage_bps > 10_000
            || self.sample_count == 0
            || self.missing_policy.trim().is_empty()
            || self.capacity_raw < 0
        {
            return Err(FactorError::Invalid("factor report 字段非法".into()));
        }
        if let Some(transform) = &self.transform {
            transform.validate()?;
        }
        Ok(())
    }

    pub fn to_json(&self) -> Result<String, FactorError> {
        self.validate()?;
        serde_json::to_string(self).map_err(|error| {
            FactorError::Invalid(format!("factor report JSON 序列化失败: {error}"))
        })
    }

    pub fn from_json(input: &str) -> Result<Self, FactorError> {
        let report: Self = serde_json::from_str(input).map_err(|error| {
            FactorError::Invalid(format!("factor report JSON 无法解析: {error}"))
        })?;
        report.validate()?;
        Ok(report)
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CandidateBinding {
    pub config: CandidateConfig,
    pub factor_keys: Vec<String>,
    pub cost_bps: u32,
    pub train_start: u64,
    pub train_end: u64,
    pub validation_start: u64,
    pub validation_end: u64,
    pub event_verified: bool,
    #[serde(default)]
    pub event_manifest_digest: Option<u64>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CandidateRequest {
    pub strategy_version: String,
    pub universe_version: String,
    pub parameters: ParameterSet,
    pub data_fingerprint: String,
    pub factor_keys: Vec<String>,
    pub cost_bps: u32,
    pub train_start: u64,
    pub train_end: u64,
    pub validation_start: u64,
    pub validation_end: u64,
    pub intended_exposure: BTreeMap<InstrumentId, i128>,
    pub constraints: BTreeMap<String, i128>,
    pub execution_model: String,
    pub risk_model: String,
}

impl CandidateBinding {
    pub fn validate_runtime(&self) -> Result<(), FactorError> {
        if self.config.strategy_version.trim().is_empty()
            || self.config.universe_version.trim().is_empty()
            || self.config.feature_version.trim().is_empty()
            || self.config.data_fingerprint.trim().is_empty()
            || self.config.execution_model.trim().is_empty()
            || self.config.risk_model.trim().is_empty()
        {
            return Err(FactorError::Invalid("candidate 缺少必填血缘字段".into()));
        }
        if self.train_start >= self.train_end
            || self.train_end > self.validation_start
            || self.validation_start >= self.validation_end
        {
            return Err(FactorError::Invalid(
                "candidate 训练/验证区间非法或重叠".into(),
            ));
        }
        if self.factor_keys.is_empty() {
            return Err(FactorError::Invalid("candidate 至少需要一个 factor".into()));
        }
        if self.event_verified != self.event_manifest_digest.is_some() {
            return Err(FactorError::Invalid(
                "event_verified 必须绑定事件回测 RunManifest".into(),
            ));
        }
        Ok(())
    }

    pub fn validate(&self, catalog: &FactorCatalog) -> Result<(), FactorError> {
        self.validate_runtime()?;
        for key in &self.factor_keys {
            if !catalog.artifacts.contains_key(key) {
                return Err(FactorError::MissingArtifact(key.clone()));
            }
        }
        Ok(())
    }

    pub fn mark_event_verified(mut self, manifest: &RunManifest) -> Result<Self, FactorError> {
        manifest
            .validate()
            .map_err(|error| FactorError::Invalid(format!("事件回测 manifest 非法: {error}")))?;
        self.event_verified = true;
        self.event_manifest_digest = Some(manifest.digest());
        Ok(self)
    }

    pub fn to_json(&self) -> Result<String, FactorError> {
        serde_json::to_string(&candidate_binding_to_wire(self))
            .map_err(|error| FactorError::Invalid(format!("candidate JSON 序列化失败: {error}")))
    }

    pub fn from_json(input: &str) -> Result<Self, FactorError> {
        let wire: CandidateBindingWire = serde_json::from_str(input)
            .map_err(|error| FactorError::Invalid(format!("candidate JSON 无法解析: {error}")))?;
        candidate_binding_from_wire(wire)
    }
}

/// 研究产物进入交易运行时的不可变输入快照。
///
/// CandidateBinding 只描述候选策略的血缘和约束，FactorReport 描述因子质量，
/// FeatureArtifact 才是某个 PIT 截止点的实际因子值。三者必须作为同一个快照
/// 进入 StrategyContext，不能让策略只读取一个脱离血缘的目标仓位文件。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StrategyResearchSnapshot {
    pub schema_version: u32,
    pub candidate: CandidateBinding,
    pub artifacts: Vec<FeatureArtifact>,
    pub reports: Vec<FactorReport>,
    pub as_of: u64,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct InstrumentValueWire {
    instrument: String,
    value: i128,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct CandidateConfigWire {
    strategy_version: String,
    universe_version: String,
    feature_version: String,
    parameters: ParameterSet,
    data_fingerprint: String,
    intended_exposure: Vec<InstrumentValueWire>,
    constraints: BTreeMap<String, i128>,
    execution_model: String,
    risk_model: String,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct CandidateBindingWire {
    config: CandidateConfigWire,
    factor_keys: Vec<String>,
    cost_bps: u32,
    train_start: u64,
    train_end: u64,
    validation_start: u64,
    validation_end: u64,
    event_verified: bool,
    event_manifest_digest: Option<u64>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct FeatureArtifactWire {
    feature_key: String,
    input_fingerprint: String,
    as_of: u64,
    coverage_bps: u32,
    values: Vec<InstrumentValueWire>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
struct StrategyResearchSnapshotWire {
    schema_version: u32,
    candidate: CandidateBindingWire,
    artifacts: Vec<FeatureArtifactWire>,
    reports: Vec<FactorReport>,
    as_of: u64,
}

fn instrument_values_to_wire(values: &BTreeMap<InstrumentId, i128>) -> Vec<InstrumentValueWire> {
    values
        .iter()
        .map(|(instrument, value)| InstrumentValueWire {
            instrument: instrument.to_string(),
            value: *value,
        })
        .collect()
}

fn instrument_values_from_wire(
    values: Vec<InstrumentValueWire>,
) -> Result<BTreeMap<InstrumentId, i128>, FactorError> {
    let mut parsed = BTreeMap::new();
    for value in values {
        let instrument = InstrumentId::parse(&value.instrument).ok_or_else(|| {
            FactorError::Invalid(format!("InstrumentId 非法: {}", value.instrument))
        })?;
        if parsed.insert(instrument, value.value).is_some() {
            return Err(FactorError::Duplicate(value.instrument));
        }
    }
    Ok(parsed)
}

fn candidate_binding_to_wire(candidate: &CandidateBinding) -> CandidateBindingWire {
    CandidateBindingWire {
        config: CandidateConfigWire {
            strategy_version: candidate.config.strategy_version.clone(),
            universe_version: candidate.config.universe_version.clone(),
            feature_version: candidate.config.feature_version.clone(),
            parameters: candidate.config.parameters.clone(),
            data_fingerprint: candidate.config.data_fingerprint.clone(),
            intended_exposure: instrument_values_to_wire(&candidate.config.intended_exposure),
            constraints: candidate.config.constraints.clone(),
            execution_model: candidate.config.execution_model.clone(),
            risk_model: candidate.config.risk_model.clone(),
        },
        factor_keys: candidate.factor_keys.clone(),
        cost_bps: candidate.cost_bps,
        train_start: candidate.train_start,
        train_end: candidate.train_end,
        validation_start: candidate.validation_start,
        validation_end: candidate.validation_end,
        event_verified: candidate.event_verified,
        event_manifest_digest: candidate.event_manifest_digest,
    }
}

fn candidate_binding_from_wire(
    wire: CandidateBindingWire,
) -> Result<CandidateBinding, FactorError> {
    let config = wire.config;
    Ok(CandidateBinding {
        config: CandidateConfig {
            strategy_version: config.strategy_version,
            universe_version: config.universe_version,
            feature_version: config.feature_version,
            parameters: config.parameters,
            data_fingerprint: config.data_fingerprint,
            intended_exposure: instrument_values_from_wire(config.intended_exposure)?,
            constraints: config.constraints,
            execution_model: config.execution_model,
            risk_model: config.risk_model,
        },
        factor_keys: wire.factor_keys,
        cost_bps: wire.cost_bps,
        train_start: wire.train_start,
        train_end: wire.train_end,
        validation_start: wire.validation_start,
        validation_end: wire.validation_end,
        event_verified: wire.event_verified,
        event_manifest_digest: wire.event_manifest_digest,
    })
}

impl StrategyResearchSnapshot {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn validate_for(
        &self,
        strategy_version: &str,
        data_fingerprint: &str,
        now: u64,
        require_event_verified: bool,
    ) -> Result<(), FactorError> {
        self.candidate.validate_runtime()?;
        if self.schema_version != Self::SCHEMA_VERSION
            || self.as_of == 0
            || self.as_of > now
            || strategy_version.trim().is_empty()
            || data_fingerprint.trim().is_empty()
        {
            return Err(FactorError::Invalid(
                "strategy research snapshot 版本、时间或运行时血缘非法".into(),
            ));
        }
        self.candidate
            .config
            .strategy_version
            .eq(strategy_version)
            .then_some(())
            .ok_or_else(|| FactorError::Invalid("candidate strategy_version 不匹配".into()))?;
        self.candidate
            .config
            .data_fingerprint
            .eq(data_fingerprint)
            .then_some(())
            .ok_or_else(|| FactorError::Invalid("candidate data_fingerprint 不匹配".into()))?;
        if require_event_verified && !self.candidate.event_verified {
            return Err(FactorError::Invalid(
                "实盘策略必须绑定已验证的事件回测 manifest".into(),
            ));
        }
        if self.candidate.factor_keys.is_empty() {
            return Err(FactorError::Invalid("candidate 没有绑定 factor".into()));
        }
        let mut artifact_by_key = BTreeMap::new();
        for artifact in &self.artifacts {
            artifact.validate()?;
            if artifact.as_of == 0
                || artifact.as_of > self.as_of
                || artifact.input_fingerprint != data_fingerprint
            {
                return Err(FactorError::Invalid(format!(
                    "artifact {} 的 PIT 时间或数据指纹不匹配",
                    artifact.feature_key
                )));
            }
            if artifact_by_key
                .insert(artifact.feature_key.clone(), artifact)
                .is_some()
            {
                return Err(FactorError::Duplicate(artifact.feature_key.clone()));
            }
        }
        let mut report_by_key = BTreeMap::new();
        for report in &self.reports {
            report.validate()?;
            if report.input_fingerprint != data_fingerprint || report.analysis_end > self.as_of {
                return Err(FactorError::Invalid(format!(
                    "report {} 的时间或数据指纹不匹配",
                    report.feature_key
                )));
            }
            if report_by_key
                .insert(report.feature_key.clone(), report)
                .is_some()
            {
                return Err(FactorError::Duplicate(report.feature_key.clone()));
            }
        }
        for key in &self.candidate.factor_keys {
            if !artifact_by_key.contains_key(key) {
                return Err(FactorError::MissingArtifact(key.clone()));
            }
            if !report_by_key.contains_key(key) {
                return Err(FactorError::MissingArtifact(format!(
                    "factor report: {key}"
                )));
            }
        }
        if self.candidate.config.intended_exposure.is_empty() {
            return Err(FactorError::Invalid(
                "运行时 candidate 必须提供已归一化的 intended_exposure".into(),
            ));
        }
        Ok(())
    }

    pub fn target_for(&self, instrument: &InstrumentId) -> Option<i128> {
        self.candidate
            .config
            .intended_exposure
            .get(instrument)
            .copied()
    }

    pub fn to_json(&self) -> Result<String, FactorError> {
        let wire = StrategyResearchSnapshotWire {
            schema_version: self.schema_version,
            candidate: CandidateBindingWire {
                config: CandidateConfigWire {
                    strategy_version: self.candidate.config.strategy_version.clone(),
                    universe_version: self.candidate.config.universe_version.clone(),
                    feature_version: self.candidate.config.feature_version.clone(),
                    parameters: self.candidate.config.parameters.clone(),
                    data_fingerprint: self.candidate.config.data_fingerprint.clone(),
                    intended_exposure: instrument_values_to_wire(
                        &self.candidate.config.intended_exposure,
                    ),
                    constraints: self.candidate.config.constraints.clone(),
                    execution_model: self.candidate.config.execution_model.clone(),
                    risk_model: self.candidate.config.risk_model.clone(),
                },
                factor_keys: self.candidate.factor_keys.clone(),
                cost_bps: self.candidate.cost_bps,
                train_start: self.candidate.train_start,
                train_end: self.candidate.train_end,
                validation_start: self.candidate.validation_start,
                validation_end: self.candidate.validation_end,
                event_verified: self.candidate.event_verified,
                event_manifest_digest: self.candidate.event_manifest_digest,
            },
            artifacts: self
                .artifacts
                .iter()
                .map(|artifact| FeatureArtifactWire {
                    feature_key: artifact.feature_key.clone(),
                    input_fingerprint: artifact.input_fingerprint.clone(),
                    as_of: artifact.as_of,
                    coverage_bps: artifact.coverage_bps,
                    values: instrument_values_to_wire(&artifact.values),
                })
                .collect(),
            reports: self.reports.clone(),
            as_of: self.as_of,
        };
        serde_json::to_string_pretty(&wire).map_err(|error| {
            FactorError::Invalid(format!(
                "strategy research snapshot JSON 序列化失败: {error}"
            ))
        })
    }

    pub fn from_json(input: &str) -> Result<Self, FactorError> {
        let wire: StrategyResearchSnapshotWire = serde_json::from_str(input).map_err(|error| {
            FactorError::Invalid(format!("strategy research snapshot JSON 无法解析: {error}"))
        })?;
        let config = wire.candidate.config;
        let candidate = CandidateBinding {
            config: CandidateConfig {
                strategy_version: config.strategy_version,
                universe_version: config.universe_version,
                feature_version: config.feature_version,
                parameters: config.parameters,
                data_fingerprint: config.data_fingerprint,
                intended_exposure: instrument_values_from_wire(config.intended_exposure)?,
                constraints: config.constraints,
                execution_model: config.execution_model,
                risk_model: config.risk_model,
            },
            factor_keys: wire.candidate.factor_keys,
            cost_bps: wire.candidate.cost_bps,
            train_start: wire.candidate.train_start,
            train_end: wire.candidate.train_end,
            validation_start: wire.candidate.validation_start,
            validation_end: wire.candidate.validation_end,
            event_verified: wire.candidate.event_verified,
            event_manifest_digest: wire.candidate.event_manifest_digest,
        };
        let artifacts = wire
            .artifacts
            .into_iter()
            .map(|artifact| {
                Ok(FeatureArtifact {
                    feature_key: artifact.feature_key,
                    input_fingerprint: artifact.input_fingerprint,
                    as_of: artifact.as_of,
                    coverage_bps: artifact.coverage_bps,
                    values: instrument_values_from_wire(artifact.values)?,
                })
            })
            .collect::<Result<Vec<_>, FactorError>>()?;
        Ok(Self {
            schema_version: wire.schema_version,
            candidate,
            artifacts,
            reports: wire.reports,
            as_of: wire.as_of,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactorError {
    Invalid(String),
    Duplicate(String),
    NonPointInTime(String),
    MissingArtifact(String),
    MissingDefinition(String),
    Cycle(String),
}

#[derive(Default)]
pub struct FactorCatalog {
    definitions: BTreeMap<String, FeatureDefinition>,
    artifacts: BTreeMap<String, FeatureArtifact>,
    reports: BTreeMap<String, FactorReport>,
}

impl FactorCatalog {
    pub fn register_definition(
        &mut self,
        definition: FeatureDefinition,
    ) -> Result<(), FactorError> {
        definition.validate()?;
        let key = definition.key();
        if self.definitions.contains_key(&key) {
            return Err(FactorError::Duplicate(key));
        }
        for dependency in &definition.dependencies {
            if !self.definitions.contains_key(dependency) {
                return Err(FactorError::MissingDefinition(dependency.clone()));
            }
        }
        self.definitions.insert(key, definition);
        Ok(())
    }

    pub fn publish_artifact(&mut self, artifact: FeatureArtifact) -> Result<(), FactorError> {
        artifact.validate()?;
        if !self.definitions.contains_key(&artifact.feature_key) {
            return Err(FactorError::MissingArtifact(artifact.feature_key));
        }
        if self.artifacts.contains_key(&artifact.feature_key) {
            return Err(FactorError::Duplicate(artifact.feature_key));
        }
        self.artifacts
            .insert(artifact.feature_key.clone(), artifact);
        Ok(())
    }

    pub fn publish_report(&mut self, report: FactorReport) -> Result<(), FactorError> {
        report.validate()?;
        if !self.artifacts.contains_key(&report.feature_key) {
            return Err(FactorError::MissingArtifact(report.feature_key));
        }
        if self.reports.contains_key(&report.feature_key) {
            return Err(FactorError::Duplicate(report.feature_key));
        }
        self.reports.insert(report.feature_key.clone(), report);
        Ok(())
    }

    pub fn bind_candidate(
        &self,
        request: CandidateRequest,
    ) -> Result<CandidateBinding, FactorError> {
        let candidate = CandidateBinding {
            config: CandidateConfig {
                strategy_version: request.strategy_version,
                universe_version: request.universe_version,
                feature_version: request.factor_keys.join("+"),
                parameters: request.parameters,
                data_fingerprint: request.data_fingerprint,
                intended_exposure: request.intended_exposure,
                constraints: request.constraints,
                execution_model: request.execution_model,
                risk_model: request.risk_model,
            },
            factor_keys: request.factor_keys,
            cost_bps: request.cost_bps,
            train_start: request.train_start,
            train_end: request.train_end,
            validation_start: request.validation_start,
            validation_end: request.validation_end,
            event_verified: false,
            event_manifest_digest: None,
        };
        candidate.validate(self)?;
        Ok(candidate)
    }

    pub fn artifact(&self, key: &str) -> Option<&FeatureArtifact> {
        self.artifacts.get(key)
    }

    pub fn feature_keys(&self) -> BTreeSet<String> {
        self.artifacts.keys().cloned().collect()
    }

    /// 返回所有已注册定义的确定性依赖顺序，供物化 Worker 使用。
    pub fn execution_order(&self) -> Result<Vec<String>, FactorError> {
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        let mut order = Vec::with_capacity(self.definitions.len());
        for key in self.definitions.keys() {
            self.visit_definition(key, &mut visiting, &mut visited, &mut order)?;
        }
        Ok(order)
    }

    fn visit_definition(
        &self,
        key: &str,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) -> Result<(), FactorError> {
        if visited.contains(key) {
            return Ok(());
        }
        if !visiting.insert(key.into()) {
            return Err(FactorError::Cycle(key.into()));
        }
        let definition = self
            .definitions
            .get(key)
            .ok_or_else(|| FactorError::MissingDefinition(key.into()))?;
        for dependency in &definition.dependencies {
            self.visit_definition(dependency, visiting, visited, order)?;
        }
        visiting.remove(key);
        visited.insert(key.into());
        order.push(key.into());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::InstrumentId;

    fn definition() -> FeatureDefinition {
        FeatureDefinition {
            name: "momentum".into(),
            version: "v1".into(),
            formula: "close / close[-20] - 1".into(),
            input_fields: vec!["close".into()],
            dependencies: Vec::new(),
            point_in_time: true,
        }
    }

    #[test]
    fn versions_and_artifacts_are_immutable() {
        let mut catalog = FactorCatalog::default();
        catalog.register_definition(definition()).unwrap();
        let instrument = InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
        catalog
            .publish_artifact(FeatureArtifact {
                feature_key: "momentum@v1".into(),
                input_fingerprint: "bars-1".into(),
                as_of: 10,
                coverage_bps: 9_800,
                values: [(instrument, 123_i128)].into_iter().collect(),
            })
            .unwrap();
        assert!(catalog
            .publish_artifact(FeatureArtifact {
                feature_key: "momentum@v1".into(),
                input_fingerprint: "bars-2".into(),
                as_of: 11,
                coverage_bps: 9_800,
                values: BTreeMap::new(),
            })
            .is_err());
    }

    #[test]
    fn reports_cannot_silently_overwrite_each_other() {
        let mut catalog = FactorCatalog::default();
        catalog.register_definition(definition()).unwrap();
        let instrument = InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
        catalog
            .publish_artifact(FeatureArtifact {
                feature_key: "momentum@v1".into(),
                input_fingerprint: "bars-1".into(),
                as_of: 10,
                coverage_bps: 10_000,
                values: [(instrument, 1)].into_iter().collect(),
            })
            .unwrap();
        let report = FactorReport {
            feature_key: "momentum@v1".into(),
            input_fingerprint: "bars-1".into(),
            observation_hash: 1,
            analysis_start: 1,
            analysis_end: 1,
            sample_count: 1,
            coverage_bps: 10_000,
            ic_bps: 1,
            rank_ic_bps: 1,
            turnover_bps: 2,
            transform: None,
            missing_policy: "reject".into(),
            decay_bps: 0,
            capacity_raw: 1,
            exposures: BTreeMap::new(),
        };
        catalog.publish_report(report.clone()).unwrap();
        assert_eq!(
            FactorReport::from_json(&report.to_json().unwrap()).unwrap(),
            report
        );
        assert!(matches!(
            catalog.publish_report(report),
            Err(FactorError::Duplicate(_))
        ));
    }

    #[test]
    fn candidate_requires_registered_factor() {
        let mut catalog = FactorCatalog::default();
        catalog.register_definition(definition()).unwrap();
        assert!(catalog
            .bind_candidate(CandidateRequest {
                strategy_version: "strategy-v1".into(),
                universe_version: "universe-v1".into(),
                parameters: ParameterSet::default(),
                data_fingerprint: "bars-1".into(),
                factor_keys: vec!["momentum@v1".into()],
                cost_bps: 8,
                train_start: 1,
                train_end: 10,
                validation_start: 11,
                validation_end: 20,
                intended_exposure: BTreeMap::new(),
                constraints: BTreeMap::new(),
                execution_model: "event-backtest@v1".into(),
                risk_model: "default-risk@v1".into(),
            },)
            .is_err());
    }

    #[test]
    fn candidate_windows_are_explicit_and_non_overlapping() {
        let mut catalog = FactorCatalog::default();
        catalog.register_definition(definition()).unwrap();
        let instrument = InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
        catalog
            .publish_artifact(FeatureArtifact {
                feature_key: "momentum@v1".into(),
                input_fingerprint: "bars-1".into(),
                as_of: 20,
                coverage_bps: 10_000,
                values: [(instrument, 1)].into_iter().collect(),
            })
            .unwrap();
        let candidate = catalog
            .bind_candidate(CandidateRequest {
                strategy_version: "strategy-v1".into(),
                universe_version: "universe-v1".into(),
                parameters: ParameterSet::default(),
                data_fingerprint: "bars-1".into(),
                factor_keys: vec!["momentum@v1".into()],
                cost_bps: 8,
                train_start: 1,
                train_end: 10,
                validation_start: 11,
                validation_end: 20,
                intended_exposure: BTreeMap::new(),
                constraints: BTreeMap::new(),
                execution_model: "event-backtest@v1".into(),
                risk_model: "default-risk@v1".into(),
            })
            .unwrap();
        assert_eq!(candidate.train_end, 10);
        assert_eq!(
            CandidateBinding::from_json(&candidate.to_json().unwrap()).unwrap(),
            candidate
        );
        let manifest = RunManifest {
            run_id: "event-run".into(),
            code_commit: "commit".into(),
            config_hash: "config".into(),
            data_fingerprint: "bars-1".into(),
            clock_start: 1,
            clock_end: 20,
            global_seed: 7,
            determinism_mode: true,
            result_hash: "result".into(),
            strategy_version: "strategy-v1".into(),
            instrument_spec_version: "instrument-v1".into(),
            model_fingerprint: "model-v1".into(),
            input_event_hash: "input".into(),
            output_event_hash: "output".into(),
            runtime_version: "runtime".into(),
        };
        let verified = candidate.clone().mark_event_verified(&manifest).unwrap();
        assert_eq!(verified.event_manifest_digest, Some(manifest.digest()));
        assert!(verified.validate(&catalog).is_ok());
        let mut invalid_manifest = manifest.clone();
        invalid_manifest.run_id.clear();
        assert!(candidate.mark_event_verified(&invalid_manifest).is_err());
        assert!(catalog
            .bind_candidate(CandidateRequest {
                strategy_version: "strategy-v1".into(),
                universe_version: "universe-v1".into(),
                parameters: ParameterSet::default(),
                data_fingerprint: "bars-1".into(),
                factor_keys: vec!["momentum@v1".into()],
                cost_bps: 8,
                train_start: 1,
                train_end: 11,
                validation_start: 10,
                validation_end: 20,
                intended_exposure: BTreeMap::new(),
                constraints: BTreeMap::new(),
                execution_model: "event-backtest@v1".into(),
                risk_model: "default-risk@v1".into(),
            },)
            .is_err());
    }

    #[test]
    fn momentum_is_point_in_time_and_transform_is_deterministic() {
        let feature = definition();
        let instrument = InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
        let view = DataView::try_new(
            vec![
                qx_guanxing::Bar::new(1, 100, 100, 100, 100, 1),
                qx_guanxing::Bar::new(2, 110, 110, 110, 110, 1),
                qx_guanxing::Bar::new(3, 120, 120, 120, 120, 1),
            ],
            qx_guanxing::DataSourceId::new("test"),
        )
        .unwrap();
        let artifact =
            compute_momentum(&feature, instrument.clone(), &view, 2, 1, "bars-v1").unwrap();
        assert_eq!(artifact.values[&instrument], 100_000_000);
        assert!(compute_momentum(&feature, instrument.clone(), &view, 1, 1, "bars-v1").is_err());
        let values = [
            (instrument.clone(), 10),
            (InstrumentId::parse("ETH-USDT.BINANCE").unwrap(), 30),
        ]
        .into_iter()
        .collect();
        let transformed = apply_transform(&values, &FactorTransform::Standardize).unwrap();
        assert_eq!(transformed[&instrument], -10);
        assert!(apply_transform(
            &values,
            &FactorTransform::Neutralize {
                group: "sector".into()
            }
        )
        .is_err());
        let groups = [
            (instrument.clone(), "A".into()),
            (InstrumentId::parse("ETH-USDT.BINANCE").unwrap(), "A".into()),
        ]
        .into_iter()
        .collect();
        let neutralized = apply_transform_with_groups(
            &values,
            &groups,
            &FactorTransform::Neutralize {
                group: "sector".into(),
            },
        )
        .unwrap();
        assert_eq!(neutralized.values().sum::<i128>(), 0);
    }

    #[test]
    fn factor_analysis_computes_cross_sectional_metrics_deterministically() {
        let instruments = [
            InstrumentId::parse("A.SIM").unwrap(),
            InstrumentId::parse("B.SIM").unwrap(),
            InstrumentId::parse("C.SIM").unwrap(),
        ];
        let mut observations = Vec::new();
        for timestamp in [1_u64, 2_u64] {
            for (index, instrument) in instruments.iter().cloned().enumerate() {
                let factor = (index + 1) as i128;
                let target = factor * 10;
                observations.push(FactorObservation {
                    timestamp,
                    instrument,
                    factor_value: Some(factor),
                    forward_returns: [(1, target), (2, 40 - target)].into_iter().collect(),
                    previous_exposure: Some(factor * 100 - 100),
                    target_exposure: Some(factor * 100),
                    capacity_raw: Some(1_000 - factor * 100),
                    exposures: [("size".into(), (factor * 100) as i32)]
                        .into_iter()
                        .collect(),
                    group_labels: [("sector".into(), "A".into())].into_iter().collect(),
                });
            }
        }
        let report = analyze_factor(
            "momentum@v1",
            &observations,
            &FactorAnalysisConfig {
                primary_horizon: 1,
                decay_horizon: Some(2),
                input_fingerprint: "bars-v1".into(),
                missing_policy: "reject".into(),
                transform: Some(FactorTransform::Standardize),
            },
        )
        .unwrap();
        assert_eq!(report.sample_count, 6);
        assert_eq!(report.coverage_bps, 10_000);
        assert_eq!(report.ic_bps, 10_000);
        assert_eq!(report.rank_ic_bps, 10_000);
        assert_eq!(report.turnover_bps, 5_000);
        assert_eq!(report.decay_bps, 20_000);
        assert_eq!(report.capacity_raw, 700);
        assert_eq!(report.exposures["size"], 200);
    }

    #[test]
    fn factor_analysis_never_silently_accepts_missing_values() {
        let instrument_a = InstrumentId::parse("A.SIM").unwrap();
        let instrument_b = InstrumentId::parse("B.SIM").unwrap();
        let rows = vec![
            FactorObservation {
                timestamp: 1,
                instrument: instrument_a,
                factor_value: Some(1),
                forward_returns: [(1, 1)].into_iter().collect(),
                previous_exposure: None,
                target_exposure: None,
                capacity_raw: None,
                exposures: BTreeMap::new(),
                group_labels: BTreeMap::new(),
            },
            FactorObservation {
                timestamp: 1,
                instrument: instrument_b,
                factor_value: None,
                forward_returns: [(1, 2)].into_iter().collect(),
                previous_exposure: None,
                target_exposure: None,
                capacity_raw: None,
                exposures: BTreeMap::new(),
                group_labels: BTreeMap::new(),
            },
        ];
        assert!(analyze_factor("momentum@v1", &rows, &FactorAnalysisConfig::default(),).is_err());
    }

    #[test]
    fn factor_dependencies_have_deterministic_topological_order() {
        let mut catalog = FactorCatalog::default();
        catalog.register_definition(definition()).unwrap();
        catalog
            .register_definition(FeatureDefinition {
                name: "ranked-momentum".into(),
                version: "v1".into(),
                formula: "rank(momentum)".into(),
                input_fields: vec!["momentum".into()],
                dependencies: vec!["momentum@v1".into()],
                point_in_time: true,
            })
            .unwrap();
        assert_eq!(
            catalog.execution_order().unwrap(),
            ["momentum@v1", "ranked-momentum@v1"]
        );
        assert!(matches!(
            catalog.register_definition(FeatureDefinition {
                name: "missing-dependency".into(),
                version: "v1".into(),
                formula: "x".into(),
                input_fields: vec!["x".into()],
                dependencies: vec!["does-not-exist@v1".into()],
                point_in_time: true,
            }),
            Err(FactorError::MissingDefinition(_))
        ));
    }

    #[test]
    fn strategy_research_snapshot_requires_complete_pit_bundle() {
        let mut catalog = FactorCatalog::default();
        catalog.register_definition(definition()).unwrap();
        let instrument = InstrumentId::parse("BTC-USDT.BINANCE").unwrap();
        let artifact = FeatureArtifact {
            feature_key: "momentum@v1".into(),
            input_fingerprint: "bars-1".into(),
            as_of: 20,
            coverage_bps: 10_000,
            values: [(instrument.clone(), 123_i128)].into_iter().collect(),
        };
        let report = FactorReport {
            feature_key: "momentum@v1".into(),
            input_fingerprint: "bars-1".into(),
            observation_hash: 7,
            analysis_start: 1,
            analysis_end: 20,
            sample_count: 20,
            coverage_bps: 10_000,
            ic_bps: 12,
            rank_ic_bps: 10,
            turnover_bps: 100,
            transform: None,
            missing_policy: "reject".into(),
            decay_bps: 0,
            capacity_raw: 1,
            exposures: BTreeMap::new(),
        };
        catalog.publish_artifact(artifact.clone()).unwrap();
        catalog.publish_report(report.clone()).unwrap();
        let candidate = catalog
            .bind_candidate(CandidateRequest {
                strategy_version: "strategy-v1".into(),
                universe_version: "universe-v1".into(),
                parameters: ParameterSet::default(),
                data_fingerprint: "bars-1".into(),
                factor_keys: vec!["momentum@v1".into()],
                cost_bps: 8,
                train_start: 1,
                train_end: 10,
                validation_start: 11,
                validation_end: 20,
                intended_exposure: [(instrument.clone(), 2)].into_iter().collect(),
                constraints: BTreeMap::new(),
                execution_model: "event-backtest@v1".into(),
                risk_model: "default-risk@v1".into(),
            })
            .unwrap();
        let snapshot = StrategyResearchSnapshot {
            schema_version: StrategyResearchSnapshot::SCHEMA_VERSION,
            candidate,
            artifacts: vec![artifact],
            reports: vec![report],
            as_of: 20,
        };
        snapshot
            .validate_for("strategy-v1", "bars-1", 21, false)
            .unwrap();
        assert_eq!(snapshot.target_for(&instrument), Some(2));
        let mut invalid = snapshot.clone();
        invalid.artifacts[0].input_fingerprint = "future-bars".into();
        assert!(invalid
            .validate_for("strategy-v1", "bars-1", 21, false)
            .is_err());
        let restored = StrategyResearchSnapshot::from_json(&snapshot.to_json().unwrap()).unwrap();
        assert_eq!(restored, snapshot);
    }
}
