use super::super::*;

pub(crate) fn strategy_account_context(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
) -> Result<StrategyAccountContext, String> {
    let account_id = config
        .strategy
        .account_id
        .as_deref()
        .ok_or_else(|| "StrategyContext 缺少 account_id".to_string())?;
    let venue_id = config
        .strategy
        .venue_id
        .as_deref()
        .ok_or_else(|| "StrategyContext 缺少 venue_id".to_string())?;
    let Some(log_name) = account_event_log_name(account_id, venue_id) else {
        return Err(format!(
            "StrategyContext 当前不支持 venue_id={} 的账户事实归约",
            venue_id
        ));
    };
    if !event_log_exists(config, root, &log_name)? {
        return Ok((
            BTreeMap::new(),
            BTreeMap::new(),
            None,
            "account-snapshot-not-seen".into(),
        ));
    }
    let pipeline = open_account_pipeline(config, root, &log_name, OutboxRecovery::ReadOnly)
        .map_err(|error| format!("恢复 StrategyContext 账户 EventLog 失败: {error}"))?;
    let state = pipeline.ledger().position_for(account_id, instrument);
    let positions = BTreeMap::from([(instrument.to_string(), state.quantity.raw())]);
    let cash = pipeline.ledger().cash_balances_for(account_id);
    let settlement = pipeline.settlement_currency();
    // Sizing uses buying power, not marked equity. Live reads the venue's free
    // balance; Paper shares the worker's market specification and valuation rule.
    let venue_balance = pipeline
        .snapshot()
        .account_balances
        .get(&(account_id.to_string(), venue_id.to_string()))
        .and_then(|balances| balances.iter().find(|balance| balance.asset == settlement))
        .map(|balance| balance.free.raw().saturating_sub(balance.borrowed.raw()));
    let (available_margin_raw, margin_provenance) = if let Some(free) = venue_balance {
        (Some(free), "venue-free-balance")
    } else if VenueFamily::parse_option(Some(venue_id)) == Some(VenueFamily::Paper) {
        let spec_paths = config
            .workers
            .iter()
            .filter(|worker| {
                worker.enabled
                    && worker.role == WorkerRole::Execution
                    && worker.account_id.as_deref() == Some(account_id)
                    && worker.venue_id.as_deref() == Some(venue_id)
                    && worker
                        .instrument_spec_path
                        .as_deref()
                        .is_some_and(|path| !path.trim().is_empty())
            })
            .filter(|worker| {
                worker.symbols.is_empty()
                    || worker
                        .symbols
                        .iter()
                        .any(|symbol| InstrumentId::parse(symbol).as_ref() == Some(instrument))
            })
            .filter_map(|worker| worker.instrument_spec_path.as_deref())
            .collect::<BTreeSet<_>>();
        if spec_paths.len() > 1 {
            return Err(format!(
                "StrategyContext 有多个 execution market spec 绑定 {}@{} / {}",
                account_id, venue_id, instrument
            ));
        }
        if let Some(spec_path) = spec_paths.into_iter().next() {
            let path = runtime_path(root, spec_path);
            let payload = std::fs::read_to_string(&path).map_err(|error| {
                format!(
                    "读取 StrategyContext market spec 失败 {}: {error}",
                    path.display()
                )
            })?;
            let market: serde_json::Value = serde_json::from_str(&payload).map_err(|error| {
                format!(
                    "StrategyContext market spec JSON 无效 {}: {error}",
                    path.display()
                )
            })?;
            let spec = market_spec_from_value(instrument, &market)?;
            (
                Some(paper_available_margin(
                    pipeline.ledger(),
                    account_id,
                    pipeline.marks(),
                    settlement,
                    &spec,
                )?),
                "paper-market-spec-buying-power",
            )
        } else {
            (
                Some(pipeline.ledger().cash_for(account_id, settlement)),
                "paper-cash-only-buying-power",
            )
        }
    } else {
        (None, "free-balance-unavailable")
    };
    let risk_state = format!("ledger-replayed-account-state+{margin_provenance}");
    Ok((positions, cash, available_margin_raw, risk_state))
}
