//! Read-only readiness report for a local Bar backtest.

use super::attach;
use super::guard::guard_panics;
use super::validate_dataset::{inspect_bar_frame, inspect_dataset};
use crate::context::{CallerCapability, RunContext};
use crate::error::AppError;
use crate::spec::{BacktestSpec, DatasetSpec};
use qx_core::{InstrumentId, Quantity};
use qx_datastruct::BarFrame;
use qx_strategy::{BuiltinStrategy, BuiltinStrategyConfig};
use serde::{Deserialize, Serialize};

pub const BACKTEST_PREFLIGHT_SCHEMA_VERSION_V1: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BacktestPreflightCheckStatusV1 {
    Ready,
    Blocked,
    NotApplicable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacktestPreflightCheckV1 {
    pub id: String,
    pub status: BacktestPreflightCheckStatusV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacktestPreflightDataV1 {
    pub instrument: String,
    pub rows: u64,
    pub content_fingerprint: String,
    pub usable: bool,
    pub gaps: Vec<String>,
    pub first_ts: Option<u64>,
    pub last_ts: Option<u64>,
}

/// Local, read-only readiness snapshot. The backtest always re-reads and validates its inputs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "BacktestPreflightResultV1Raw")]
pub struct BacktestPreflightResultV1 {
    pub schema_version: u32,
    pub run_id: String,
    pub instrument: String,
    pub ready: bool,
    pub data: BacktestPreflightDataV1,
    pub checks: Vec<BacktestPreflightCheckV1>,
    pub limitations: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BacktestPreflightResultV1Raw {
    schema_version: u32,
    run_id: String,
    instrument: String,
    ready: bool,
    data: BacktestPreflightDataV1,
    checks: Vec<BacktestPreflightCheckV1>,
    limitations: Vec<String>,
}

impl TryFrom<BacktestPreflightResultV1Raw> for BacktestPreflightResultV1 {
    type Error = &'static str;

    fn try_from(raw: BacktestPreflightResultV1Raw) -> Result<Self, Self::Error> {
        let result = Self {
            schema_version: raw.schema_version,
            run_id: raw.run_id,
            instrument: raw.instrument,
            ready: raw.ready,
            data: raw.data,
            checks: raw.checks,
            limitations: raw.limitations,
        };
        result.validate()?;
        Ok(result)
    }
}

impl BacktestPreflightResultV1 {
    /// Validate cross-field invariants that JSON's structural deserializer cannot enforce.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != BACKTEST_PREFLIGHT_SCHEMA_VERSION_V1 {
            return Err("unsupported backtest preflight schema version");
        }
        if self.ready
            != self
                .checks
                .iter()
                .all(|check| check.status != BacktestPreflightCheckStatusV1::Blocked)
        {
            return Err("ready flag disagrees with blocked checks");
        }
        if self.data.usable != self.data.gaps.is_empty() {
            return Err("data usable flag disagrees with dataset gaps");
        }
        if matches!((self.data.first_ts, self.data.last_ts), (Some(first), Some(last)) if first > last)
        {
            return Err("data first_ts must not be after last_ts");
        }
        Ok(())
    }
}

/// Check local Bar data, its identity and explicit market-rule date coverage before a run.
pub fn preflight_backtest_v1(
    spec: &BacktestSpec,
    context: &RunContext,
) -> Result<BacktestPreflightResultV1, AppError> {
    context.require(CallerCapability::Research, "Bar 回测预检")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        spec.validate()?;
        let inspection = inspect_dataset(&DatasetSpec::new(&spec.run_id, &spec.bars_path))?;
        preflight_inner(spec, inspection)
    })
    .map_err(|error| attach(error, &correlation_id))
}

/// Build the preflight report from a BarFrame that the caller has already read and verified
/// against `spec.bars_path`. This is intended for the backtest execution path, which already
/// owns that exact immutable input snapshot and should not parse it again just to report
/// readiness.
pub fn preflight_backtest_v1_with_frame(
    spec: &BacktestSpec,
    frame: &BarFrame,
    context: &RunContext,
) -> Result<BacktestPreflightResultV1, AppError> {
    context.require(CallerCapability::Research, "Bar 回测预检")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        spec.validate()?;
        let inspection = inspect_bar_frame(&spec.run_id, frame)?;
        preflight_inner(spec, inspection)
    })
    .map_err(|error| attach(error, &correlation_id))
}

fn preflight_inner(
    spec: &BacktestSpec,
    inspection: super::validate_dataset::DatasetInspection,
) -> Result<BacktestPreflightResultV1, AppError> {
    let requested = InstrumentId::parse(&spec.instrument).ok_or_else(|| {
        crate::error::AppError::new(
            crate::error::AppErrorCategory::InvalidInput,
            "BacktestSpec.instrument 在预检后无法解析",
        )
    })?;
    let dataset = inspection.verdict;
    let instrument_matches = requested == inspection.instrument;
    let mut strategies = vec![&spec.strategy];
    if let Some(portfolio) = &spec.portfolio {
        strategies.extend(
            portfolio
                .additional_strategies
                .iter()
                .map(|member| &member.strategy),
        );
    }
    let strategy_history = strategies
        .iter()
        .map(|strategy| {
            minimum_history_bars(strategy, &requested, spec.allow_short.unwrap_or(false))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let short_strategies: Vec<_> = strategy_history
        .iter()
        .filter(|(_, required_bars)| {
            u64::try_from(*required_bars).unwrap_or(u64::MAX) > dataset.rows
        })
        .cloned()
        .collect();
    let history_ready = short_strategies.is_empty();
    let history_detail = (!history_ready).then(|| {
        short_strategies
            .iter()
            .map(|(strategy_id, required_bars)| {
                format!(
                    "策略 {strategy_id} 至少需要 {required_bars} 根 Bar，当前 {} 根",
                    dataset.rows
                )
            })
            .collect::<Vec<_>>()
            .join("；")
    });
    let cost_inputs_explicit = spec.fee_schedule.is_some()
        || spec.execution_cost_rules.is_some()
        || spec.ashare_rules.is_some();
    let mut checks = vec![
        BacktestPreflightCheckV1 {
            id: "dataset_sample".into(),
            status: if dataset.usable {
                BacktestPreflightCheckStatusV1::Ready
            } else {
                BacktestPreflightCheckStatusV1::Blocked
            },
            detail: (!dataset.gaps.is_empty()).then(|| dataset.gaps.join("；")),
        },
        BacktestPreflightCheckV1 {
            id: "instrument_identity".into(),
            status: if instrument_matches {
                BacktestPreflightCheckStatusV1::Ready
            } else {
                BacktestPreflightCheckStatusV1::Blocked
            },
            detail: (!instrument_matches).then(|| {
                format!(
                    "数据标的 {} 与请求标的 {} 不一致",
                    inspection.instrument, requested
                )
            }),
        },
        BacktestPreflightCheckV1 {
            id: "strategy_history".into(),
            status: if history_ready {
                BacktestPreflightCheckStatusV1::Ready
            } else {
                BacktestPreflightCheckStatusV1::Blocked
            },
            detail: history_detail,
        },
        BacktestPreflightCheckV1 {
            id: "execution_cost_inputs".into(),
            status: if cost_inputs_explicit {
                BacktestPreflightCheckStatusV1::Ready
            } else {
                BacktestPreflightCheckStatusV1::NotApplicable
            },
            detail: (!cost_inputs_explicit)
                .then(|| "未声明显式费用/执行成本；回测沿用兼容默认口径".into()),
        },
    ];

    match (&spec.market_spec, inspection.first_ts, inspection.last_ts) {
        (None, _, _) => checks.push(BacktestPreflightCheckV1 {
            id: "market_rule_window".into(),
            status: BacktestPreflightCheckStatusV1::NotApplicable,
            detail: Some("未提供冻结市场规则，运行将沿用兼容行为".into()),
        }),
        (Some(_), None, _) | (Some(_), _, None) => checks.push(BacktestPreflightCheckV1 {
            id: "market_rule_window".into(),
            status: BacktestPreflightCheckStatusV1::Blocked,
            detail: Some("数据没有时间戳，无法与市场规则有效期核对".into()),
        }),
        (Some(market), Some(first_ts), Some(last_ts)) => {
            let covered = first_ts >= market.valid_from
                && market.valid_to.is_none_or(|valid_to| last_ts < valid_to);
            checks.push(BacktestPreflightCheckV1 {
                id: "market_rule_window".into(),
                status: if covered {
                    BacktestPreflightCheckStatusV1::Ready
                } else {
                    BacktestPreflightCheckStatusV1::Blocked
                },
                detail: (!covered).then(|| {
                    format!(
                        "数据时间范围 [{first_ts}, {last_ts}] 不完全落在市场规则有效期 [{}, {}) 内",
                        market.valid_from,
                        market
                            .valid_to
                            .map_or_else(|| "∞".into(), |value| value.to_string())
                    )
                }),
            });
        }
    }

    let ready = checks
        .iter()
        .all(|check| check.status != BacktestPreflightCheckStatusV1::Blocked);
    Ok(BacktestPreflightResultV1 {
        schema_version: BACKTEST_PREFLIGHT_SCHEMA_VERSION_V1,
        run_id: spec.run_id.clone(),
        instrument: spec.instrument.clone(),
        ready,
        data: BacktestPreflightDataV1 {
            instrument: dataset.instrument,
            rows: dataset.rows,
            content_fingerprint: dataset.content_fingerprint,
            usable: dataset.usable,
            gaps: dataset.gaps,
            first_ts: inspection.first_ts,
            last_ts: inspection.last_ts,
        },
        checks,
        limitations: vec![
            "此预检只检查 BacktestSpec 与本地 Bar 数据，不检查 Provider 网络、账户凭据或交易所权限。".into(),
            "这是只读数据快照；预检后文件若发生变化，实际回测仍会重新读取并校验。".into(),
            "预检不创建输出目录，也不保证目标磁盘路径可写。".into(),
            "ready 只表示这些静态输入检查通过，不保证策略产生信号、成交或正收益。".into(),
            "预检只读取 BacktestSpec 中的内置策略参数；用户自定义 Strategy 的 warmup 由该策略自身声明并在运行时检查。".into(),
            "未提供显式费用/执行成本规则时，回测使用兼容默认值；预检不估算账户等级、返佣或实时费率。".into(),
        ],
    })
}

fn minimum_history_bars(
    spec: &crate::spec::BuiltinStrategySpec,
    instrument: &InstrumentId,
    allow_short: bool,
) -> Result<(String, usize), AppError> {
    let mut config = BuiltinStrategyConfig::new(
        spec.kind,
        spec.strategy_id.clone(),
        instrument.clone(),
        Quantity::from_raw(spec.quantity_raw),
    )
    .map_err(|message| {
        AppError::new(
            crate::error::AppErrorCategory::InvalidInput,
            format!("内置策略预检配置无效: {message}"),
        )
    })?;
    config.fast_window = spec.fast_window;
    config.slow_window = spec.slow_window;
    config.period = spec.period;
    config.threshold_bps = spec.threshold_bps;
    config.target_notional_raw = spec.target_notional_raw;
    config.target_available_margin_bps = spec.target_available_margin_bps;
    config.target_volatility_bps = spec.target_volatility_bps;
    config.target_kelly_fraction_bps = spec.target_kelly_fraction_bps;
    config.kelly_lookback_bars = spec.kelly_lookback_bars;
    config.allow_short = allow_short;
    config.validate().map_err(|message| {
        AppError::new(
            crate::error::AppErrorCategory::InvalidInput,
            format!("内置策略预检配置无效: {message}"),
        )
    })?;
    let strategy = BuiltinStrategy::new(config).map_err(|message| {
        AppError::new(
            crate::error::AppErrorCategory::InvalidInput,
            format!("内置策略预检初始化失败: {message}"),
        )
    })?;
    Ok((spec.strategy_id.clone(), strategy.minimum_history_bars()))
}
