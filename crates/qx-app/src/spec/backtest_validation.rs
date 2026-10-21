use super::validation::is_identity_token;
use crate::error::{AppError, AppErrorCategory};
use crate::spec::{BacktestSpec, BACKTEST_SPEC_SCHEMA_VERSION};

pub(super) fn validate(spec: &BacktestSpec) -> Result<(), AppError> {
    if spec.schema_version != BACKTEST_SPEC_SCHEMA_VERSION {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!(
                "BacktestSpec schema_version={} 本版本只认 {}",
                spec.schema_version, BACKTEST_SPEC_SCHEMA_VERSION
            ),
        ));
    }
    for (field, value) in [
        ("instrument", &spec.instrument),
        ("bars_path", &spec.bars_path),
        ("settlement_currency", &spec.settlement_currency),
        ("output_dir", &spec.output_dir),
    ] {
        if value.trim().is_empty() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!("BacktestSpec.{field} 不能为空"),
            ));
        }
    }
    if !is_identity_token(&spec.run_id) {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!(
                "BacktestSpec.run_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符（它会变成产物文件名），实际 {:?}",
                spec.run_id
            ),
        ));
    }
    if spec
        .account_id
        .as_deref()
        .is_some_and(|account_id| !is_identity_token(account_id))
    {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "BacktestSpec.account_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符",
        ));
    }
    if qx_core::InstrumentId::parse(&spec.instrument).is_none() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!("BacktestSpec.instrument 非法: {}", spec.instrument),
        ));
    }
    if spec.initial_cash_raw <= 0 {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "BacktestSpec.initial_cash_raw 必须为正",
        ));
    }
    if let Some(market) = &spec.market_spec {
        market
            .validate()
            .map_err(|error| AppError::from_qx_error(&error))?;
        if market.product == qx_core::TradingProduct::Option {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "Bar 回测暂不接收期权规格：行权价、到期日与认购/认沽身份尚未建模",
            ));
        }
        if qx_core::InstrumentId::parse(&spec.instrument).as_ref() != Some(&market.instrument) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BacktestSpec.market_spec.instrument 必须与 BacktestSpec.instrument 一致",
            ));
        }
        if market.settlement_currency != spec.settlement_currency {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BacktestSpec.market_spec.settlement_currency 必须与 settlement_currency 一致",
            ));
        }
        if spec.fee_schedule.is_none()
            && spec.execution_cost_rules.is_none()
            && spec.ashare_rules.is_none()
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "指定 market_spec 时必须显式提供 fee_schedule、execution_cost_rules 或 ashare_rules",
            ));
        }
    }
    if let Some(fees) = spec.fee_schedule {
        fees.validate()
            .map_err(|error| AppError::from_qx_error(&error))?;
    }
    if let Some(costs) = &spec.execution_cost_rules {
        costs
            .validate()
            .map_err(|error| AppError::from_qx_error(&error))?;
    }
    if let Some(rules) = &spec.ashare_rules {
        rules.validate().map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("BacktestSpec.ashare_rules 非法: {error}"),
            )
        })?;
        if !rules.enabled {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BacktestSpec.ashare_rules.enabled 必须为 true",
            ));
        }
    }
    if spec.fill_model == Some(crate::spec::BacktestFillModel::OneTickSlippage)
        && spec.market_spec.is_none()
    {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "BacktestSpec.fill_model=one_tick_slippage 需要声明 market_spec.price_tick",
        ));
    }
    if spec
        .max_bar_volume_participation_bps
        .is_some_and(|bps| bps == 0 || bps > 10_000)
    {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "BacktestSpec.max_bar_volume_participation_bps 必须在 1..=10000 内",
        ));
    }
    if let Some(rules) = &spec.risk_rules {
        if rules.version.trim().is_empty() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BacktestSpec.risk_rules.version 不能为空",
            ));
        }
        if rules.max_qty_raw.is_some_and(|value| value <= 0) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BacktestSpec.risk_rules.max_qty_raw 必须为正",
            ));
        }
        if rules.max_notional_raw.is_some_and(|value| value <= 0) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BacktestSpec.risk_rules.max_notional_raw 必须为正",
            ));
        }
    }
    if let Some(margin) = &spec.margin_model {
        margin
            .validate()
            .map_err(|message| AppError::new(AppErrorCategory::InvalidInput, message))?;
        if spec.market_spec.is_none() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BacktestSpec.margin_model 需要 market_spec",
            ));
        }
    }
    if let Some(policy) = spec.protective_exit {
        policy.validate()?;
    }
    spec.strategy.validate()?;
    if let Some(portfolio) = &spec.portfolio {
        if spec.protective_exit.is_some() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "策略组合暂不支持账户净持仓级 protective_exit，避免退出成交无法归属到策略持仓",
            ));
        }
        portfolio.validate(&spec.strategy, spec.initial_cash_raw)?;
    }
    Ok(())
}
