use crate::*;

pub(crate) fn resolve_worker_runtime_paths(worker: &mut WorkerConfig, runtime_path: &Path) {
    if let Some(spec_path) = worker.instrument_spec_path.as_mut() {
        *spec_path = resolve_runtime_relative_path(runtime_path, spec_path)
            .to_string_lossy()
            .into_owned();
    }
    if let Some(files) = worker.credential_files.as_mut() {
        files.api_key = resolve_runtime_relative_path(runtime_path, &files.api_key)
            .to_string_lossy()
            .into_owned();
        files.secret = resolve_runtime_relative_path(runtime_path, &files.secret)
            .to_string_lossy()
            .into_owned();
    }
}

#[cfg(test)]
pub(crate) use qx_runtime::owns_account_event_log;
#[cfg(test)]
pub(crate) use qx_runtime::worker_settlement_currency;
pub(crate) use qx_runtime::{
    account_worker_settlement_currency, check_account_log_settlement, writes_account_ledger,
};

/// Paper 账户可用保证金的估值口径，必须和 `RiskContext` 里的 `initial_margin` 同一把尺子：
/// 现货买入付出现金，持仓按全额名义额计入权益才是账户价值；保证金产品开仓不动现金，
/// 账上属于这笔持仓的只有已实现/未实现 PnL，再按名义额累加等于凭空多出整笔可用保证金
/// （10k USDT 开 1 张 50k 名义的永续会把权益算成 60k，10 倍杠杆规则形同虚设，且每成交
/// 一次就更宽松）。缺少任一持仓规格、标记价格或币种汇率时失败关闭，不以现金余额代替估值。
pub(crate) fn paper_available_margin(
    ledger: &Ledger,
    account_id: &str,
    marks: &BTreeMap<InstrumentId, Price>,
    settlement: &str,
    spec: &TradingInstrumentSpec,
) -> Result<i128, String> {
    let market_specs = BTreeMap::from([(spec.instrument.clone(), spec.clone())]);
    qx_app::paper_available_margin_raw(
        ledger,
        account_id,
        marks,
        settlement,
        &market_specs,
        &BTreeMap::new(),
        &qx_app::RunContext::new(
            qx_app::CallerCapability::Paper,
            format!("paper-risk:{account_id}"),
        ),
    )
    .map_err(|error| format!("Paper 账户可用保证金计算失败: {error}"))
}

pub(crate) fn load_worker_instrument_spec(
    worker: &WorkerConfig,
    order: &Order,
    runtime_config_path: Option<&Path>,
) -> Result<Option<TradingInstrumentSpec>, String> {
    load_worker_market_spec_for_instrument(worker, &order.instrument, runtime_config_path)
}

#[cfg(test)]
pub(crate) use qx_service::worker_market_spec::worker_report_spec;

/// 提交前置门槛：没有冻结的 market spec 就没有账户级风控，实盘链一律拒绝，
/// 不再降级为无风控提交；放在 venue/凭据装配之前，让未配置的风控边界在
/// 任何外部动作发生前就失败。
pub(crate) fn require_worker_risk_spec(
    worker: &WorkerConfig,
    command: &ControlCommand,
    runtime_config_path: Option<&Path>,
) -> Result<(), String> {
    let order = order_from_submit_command(command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    if load_worker_instrument_spec(worker, &order, runtime_config_path)?.is_some() {
        return Ok(());
    }
    Err(format!(
        "FAIL_CLOSED: worker {} 缺少风控配置（instrument_spec_path），拒绝提交订单",
        worker.id
    ))
}
