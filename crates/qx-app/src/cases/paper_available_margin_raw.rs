//! Paper 可用保证金用例：在应用边界统一注入 qx-core 账簿估值结果。

use crate::cases::attach;
use crate::cases::guard::guard_panics;
use crate::{AppError, AppErrorCategory, CallerCapability, RunContext};
use qx_core::{InstrumentId, Ledger, PaperValuationContext, Price, TradingInstrumentSpec};
use std::collections::BTreeMap;

/// 按账户账簿币种计算 Paper 可用保证金，供各 Paper 风控入口共享使用。
///
/// 需要 Paper 能力；持仓标记、冻结市场规格与 FX 缺失均沿用 `Ledger::valuate_paper` 的 fail-closed
/// 行为。用例只负责应用层授权、输入边界、panic 保护和错误关联。
pub fn paper_available_margin_raw(
    ledger: &Ledger,
    account_id: &str,
    marks: &BTreeMap<InstrumentId, Price>,
    reporting_currency: &str,
    market_specs: &BTreeMap<InstrumentId, TradingInstrumentSpec>,
    fx_rates: &BTreeMap<String, Price>,
    context: &RunContext,
) -> Result<i128, AppError> {
    let correlation_id = context.correlation_id().to_string();
    let authorization = context.require(CallerCapability::Paper, "Paper 账户估值");
    authorization.map_err(|error| attach(error, &correlation_id))?;
    if account_id.trim().is_empty() || reporting_currency.trim().is_empty() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "Paper 账户估值需要非空账户 ID 和记账币种",
        )
        .with_correlation_id(&correlation_id));
    }

    let result = guard_panics(&correlation_id, || {
        ledger
            .valuate_paper(&PaperValuationContext::new(
                account_id,
                marks,
                reporting_currency,
                market_specs,
                fx_rates,
            ))
            .map(|result| result.available.raw())
            .map_err(|error| AppError::from_qx_error(&error))
    });
    result.map_err(|error| attach(error, &correlation_id))
}
