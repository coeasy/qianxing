//! Account-isolated exchange portfolios with synchronized aggregate reporting.

use super::artifacts::{read_text, write_text};
use super::attach;
use super::guard::guard_panics;
use super::run_multi_market_backtest::run_inner as run_market_inner;
use crate::{
    AppError, AppErrorCategory, CallerCapability, JointAccountBacktestResult,
    MultiAccountJointBacktestOutcome, MultiAccountJointBacktestSpec, MultiMarketBacktestSpec,
    RunContext,
};
use qx_core::Fnv1a;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub fn run_multi_account_joint_backtest(
    spec: &MultiAccountJointBacktestSpec,
    context: &RunContext,
) -> Result<MultiAccountJointBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "多交易所多账户联合 Bar 回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || run_inner(spec, context, None))
        .map_err(|error| attach(error, &correlation_id))?
        .ok_or_else(|| {
            attach(
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "同步联合账户回测意外返回取消状态",
                ),
                &correlation_id,
            )
        })
}

impl MultiAccountJointBacktestSpec {
    pub fn start(self, context: RunContext) -> crate::RunHandle<MultiAccountJointBacktestOutcome> {
        let run_id = self.run_id.clone();
        crate::RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "多交易所多账户联合 Bar 回测")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner(&self, &context, Some(&cancellation))
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }
}

fn run_inner(
    spec: &MultiAccountJointBacktestSpec,
    context: &RunContext,
    cancellation: Option<&crate::run_handle::CancellationToken>,
) -> Result<Option<MultiAccountJointBacktestOutcome>, AppError> {
    if cancellation.is_some_and(|token| token.is_cancelled()) {
        return Ok(None);
    }
    spec.validate()?;

    let mut canonical_accounts = spec.accounts.clone();
    canonical_accounts.sort_by(|left, right| {
        left.exchange_id
            .cmp(&right.exchange_id)
            .then_with(|| left.account_id.cmp(&right.account_id))
    });
    let mut canonical = spec.clone();
    canonical.accounts = canonical_accounts.clone();
    canonical.output_dir.clear();
    for account in &mut canonical.accounts {
        for market in &mut account.markets {
            market.bars_path.clear();
            market.dataset_manifest_path = None;
        }
    }
    let canonical_json = canonical.to_json()?;
    let mut spec_hasher = Fnv1a::new();
    spec_hasher.write_text(&canonical_json);
    let spec_fingerprint = format!("{:016x}", spec_hasher.finish());

    let mut accounts = Vec::with_capacity(canonical_accounts.len());
    let mut curves = BTreeMap::<(String, String), (i128, Vec<(u64, i128)>)>::new();
    let mut fills = Vec::<Value>::new();
    let mut total_cash_raw = 0_i128;
    let mut markets_total = 0_usize;
    let mut successful_fills = 0_u64;
    for (index, account) in canonical_accounts.iter().enumerate() {
        if cancellation.is_some_and(|token| token.is_cancelled()) {
            return Ok(None);
        }
        let child_run_id = child_run_id(&spec.run_id, &account.exchange_id, &account.account_id);
        let child_spec = MultiMarketBacktestSpec {
            schema_version: crate::MULTI_MARKET_BACKTEST_SCHEMA_VERSION,
            run_id: child_run_id.clone(),
            account_id: account.account_id.clone(),
            settlement_currency: spec.settlement_currency.clone(),
            initial_cash_raw: account.initial_cash_raw,
            seed: spec.seed,
            output_dir: spec.output_dir.clone(),
            markets: account.markets.clone(),
        };
        let member_context = RunContext::new(
            context.capability(),
            format!("{}-account-{index}", context.correlation_id()),
        )
        .with_code_commit(context.code_commit());
        let child_outcome =
            run_market_inner(&child_spec, &member_context, cancellation, BTreeMap::new())?
                .ok_or_else(|| invalid("联合账户成员回测被取消"))?;

        let root = PathBuf::from(&spec.output_dir);
        let curve_path = root.join(format!("{child_run_id}.multi-market.equity.json"));
        let fills_path = root.join(format!("{child_run_id}.multi-market.fills.json"));
        let curve: Vec<(u64, i128)> =
            serde_json::from_str(&read_text(&curve_path.to_string_lossy())?).map_err(|error| {
                invalid(format!(
                    "联合账户 {} 权益曲线格式非法: {error}",
                    account.account_id
                ))
            })?;
        let account_fills: Vec<Value> =
            serde_json::from_str(&read_text(&fills_path.to_string_lossy())?).map_err(|error| {
                invalid(format!(
                    "联合账户 {} 成交明细格式非法: {error}",
                    account.account_id
                ))
            })?;
        if curve.is_empty() {
            return Err(invalid(format!(
                "联合账户 {} 没有权益曲线",
                account.account_id
            )));
        }
        total_cash_raw = total_cash_raw
            .checked_add(account.initial_cash_raw)
            .ok_or_else(|| invalid("联合账户初始资金合计溢出"))?;
        markets_total = markets_total
            .checked_add(account.markets.len())
            .ok_or_else(|| invalid("联合账户市场数溢出"))?;
        successful_fills = successful_fills
            .checked_add(child_outcome.successful_fills)
            .ok_or_else(|| invalid("联合账户成交数溢出"))?;
        fills.extend(account_fills);
        curves.insert(
            (account.exchange_id.clone(), account.account_id.clone()),
            (account.initial_cash_raw, curve),
        );
        accounts.push(JointAccountBacktestResult {
            exchange_id: account.exchange_id.clone(),
            account_id: account.account_id.clone(),
            initial_cash_raw: account.initial_cash_raw,
            outcome: child_outcome,
        });
    }

    if cancellation.is_some_and(|token| token.is_cancelled()) {
        return Ok(None);
    }
    let mut timestamps = curves
        .values()
        .flat_map(|(_, curve)| curve.iter().map(|(ts, _)| *ts))
        .collect::<Vec<_>>();
    timestamps.sort_unstable();
    timestamps.dedup();
    let mut combined_equity = Vec::with_capacity(timestamps.len());
    for ts in timestamps {
        let mut total = 0_i128;
        for (initial, curve) in curves.values() {
            let end = curve.partition_point(|(point_ts, _)| *point_ts <= ts);
            let value = end
                .checked_sub(1)
                .and_then(|index| curve.get(index))
                .map(|(_, equity)| *equity)
                .unwrap_or(*initial);
            total = total
                .checked_add(value)
                .ok_or_else(|| invalid("联合权益合计溢出"))?;
        }
        combined_equity.push((ts, total));
    }
    let final_equity_raw = combined_equity
        .last()
        .map(|(_, value)| *value)
        .ok_or_else(|| invalid("联合权益曲线为空"))?;
    let return_bps = final_equity_raw
        .checked_sub(total_cash_raw)
        .and_then(|pnl| pnl.checked_mul(10_000))
        .and_then(|value| value.checked_div(total_cash_raw))
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| invalid("联合账户收益无法表示"))?;
    let max_drawdown_bps = max_drawdown_bps(&combined_equity)?;
    let mut result_hasher = Fnv1a::new();
    result_hasher.write_text(&spec_fingerprint);
    for account in &accounts {
        result_hasher.write_text(&account.outcome.shared_result_hash);
    }
    let result_hash = format!("{:016x}", result_hasher.finish());

    std::fs::create_dir_all(&spec.output_dir)
        .map_err(|error| AppError::from_io("创建联合账户回测产物目录", &error))?;
    let output_root = PathBuf::from(&spec.output_dir)
        .canonicalize()
        .map_err(|error| AppError::from_io("解析联合账户产物目录", &error))?;
    let equity_path = output_root.join(format!("{}.joint-equity.json", spec.run_id));
    let fills_path = output_root.join(format!("{}.joint-fills.json", spec.run_id));
    let artifact_path = output_root.join(format!("{}.joint-portfolio.json", spec.run_id));
    let equity_json = serde_json::to_string(&combined_equity)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let fills_json = serde_json::to_string(&fills)
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    let outcome = MultiAccountJointBacktestOutcome {
        run_id: spec.run_id.clone(),
        settlement_currency: spec.settlement_currency.clone(),
        spec_fingerprint,
        accounts_total: accounts.len(),
        markets_total,
        initial_cash_raw: total_cash_raw,
        final_equity_raw,
        return_bps,
        max_drawdown_bps,
        successful_fills,
        equity_path: equity_path.to_string_lossy().into_owned(),
        fills_path: fills_path.to_string_lossy().into_owned(),
        artifact_path: artifact_path.to_string_lossy().into_owned(),
        accounts,
    };
    let artifact_json = serde_json::to_string(&serde_json::json!({
        "schema_version": 1,
        "spec": spec,
        "outcome": outcome,
        "shared_result_hash": result_hash,
        "joint_equity_path": outcome.equity_path,
        "joint_fills_path": outcome.fills_path,
        "aggregation": "sum_of_independent_exchange_account_equity_no_cross_account_margin_netting",
    }))
    .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    for (path, contents, label) in [
        (&equity_path, equity_json.as_str(), "联合权益曲线"),
        (&fills_path, fills_json.as_str(), "联合成交明细"),
        (&artifact_path, artifact_json.as_str(), "联合组合产物"),
    ] {
        match std::fs::read_to_string(path) {
            Ok(existing) if existing == contents => {}
            Ok(_) => {
                return Err(AppError::new(
                    AppErrorCategory::Conflict,
                    format!("联合账户回测 {label} 已存在且内容不同: {}", path.display()),
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                write_text(&path.to_string_lossy(), contents)?
            }
            Err(error) => return Err(AppError::from_io(&format!("读取联合账户 {label}"), &error)),
        }
    }
    Ok(Some(outcome))
}

fn child_run_id(run_id: &str, exchange_id: &str, account_id: &str) -> String {
    let mut hasher = Fnv1a::new();
    hasher.write_text(run_id);
    hasher.write_text(exchange_id);
    hasher.write_text(account_id);
    format!("joint-{:016x}", hasher.finish())
}

fn max_drawdown_bps(equity: &[(u64, i128)]) -> Result<u32, AppError> {
    let mut peak = 0_i128;
    let mut max = 0_u128;
    for (_, value) in equity {
        peak = peak.max(*value);
        if peak <= 0 {
            continue;
        }
        let drawdown = peak
            .checked_sub(*value)
            .ok_or_else(|| invalid("联合权益回撤溢出"))?;
        let bps = u128::try_from(drawdown)
            .ok()
            .and_then(|drawdown| drawdown.checked_mul(10_000))
            .and_then(|scaled| scaled.checked_div(peak as u128))
            .ok_or_else(|| invalid("联合权益回撤无法表示"))?;
        max = max.max(bps);
    }
    u32::try_from(max).map_err(|_| invalid("联合最大回撤超出 u32 范围"))
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
