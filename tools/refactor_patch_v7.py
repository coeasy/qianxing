from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    if new in text:
        return
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected exactly one guarded match, got {count}")
    p.write_text(text.replace(old, new, 1))


main = "crates/qx-cli/src/main.rs"
replace_once(
    main,
    """    run_scheduler_worker(path, &scheduler_id, true)?;
    run_strategy_worker(path, &strategy_id, true)?;
    run_paper_execution_worker(path, &execution_id, true)?;

    let root = Path::new(&config.storage.data_dir);
""",
    """    run_scheduler_worker(path, &scheduler_id, true)?;
    run_strategy_worker(path, &strategy_id, true)?;

    // The offline Paper E2E owns an explicit deterministic market-data fixture.
    // Do not let RiskContext invent a reference price: market orders may execute
    // only after the standard live pipeline has observed a MarketQuote fact.
    {
        let root = Path::new(&config.storage.data_dir);
        let worker = config
            .workers
            .iter()
            .find(|worker| worker.id == execution_id)
            .ok_or_else(|| "Paper Execution worker 配置在行情注入前消失".to_string())?;
        let strategy = config.strategy_for_worker(&strategy_id)?;
        let instrument = strategy
            .instrument
            .as_deref()
            .and_then(InstrumentId::parse)
            .ok_or_else(|| "Paper E2E Strategy 缺少合法 instrument".to_string())?;
        let log_name = format!(
            "paper-{}-{}-events",
            worker.account_id.as_deref().unwrap_or("unknown"),
            worker.venue_id.as_deref().unwrap_or("paper")
        );
        let now = runtime_timestamp_ms();
        let mut pipeline = open_runtime_pipeline(&config, root, &log_name, "USDT")
            .map_err(|error| format!("打开 Paper E2E 行情 EventLog 失败: {error}"))?;
        pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                instrument,
                Price::from_i64(99),
                Price::from_i64(100),
                now,
                now,
                0,
                "paper-e2e-market-fixture",
            ))
            .map_err(|error| format!("写入 Paper E2E 行情事实失败: {error:?}"))?;
    }

    run_paper_execution_worker(path, &execution_id, true)?;

    let root = Path::new(&config.storage.data_dir);
""",
)

replace_once(
    main,
    """                queue
                    .ack_command_at(command.command_id, context.id(), lease.fencing_token, now)
                    .map_err(|error| format!("确认 Paper SubmitOrder 失败: {error:?}"))?;
                processed += 1;
                context.mark(
""",
    """                queue
                    .ack_command_at(command.command_id, context.id(), lease.fencing_token, now)
                    .map_err(|error| format!("确认 Paper SubmitOrder 失败: {error:?}"))?;
                if once && record.status == qx_control::CommandStatus::Failed {
                    return Err(format!(
                        "Paper SubmitOrder command_id={} 执行失败: {}",
                        command.command_id, record.result_code
                    ));
                }
                processed += 1;
                context.mark(
""",
)

runtime = "crates/qx-runtime/src/lib.rs"
replace_once(
    runtime,
    """            if worker.role == WorkerRole::Execution
                && !worker
                    .venue_id
                    .as_deref()
                    .is_some_and(|venue| venue.eq_ignore_ascii_case("paper"))
                && worker.instrument_spec_path.is_none()
            {
                return Err(format!(
                    "{} 非 Paper Execution worker 必须配置 instrument_spec_path",
                    worker.id
                ));
            }
""",
    """            if worker.role == WorkerRole::Execution && worker.instrument_spec_path.is_none() {
                return Err(format!(
                    "{} Execution worker 必须配置冻结的 instrument_spec_path",
                    worker.id
                ));
            }
""",
)

replace_once(
    runtime,
    """    #[test]
    fn execution_risk_limits_require_a_frozen_instrument_spec() {
""",
    """    #[test]
    fn paper_execution_requires_a_frozen_instrument_spec_even_without_limits() {
        let mut config = config();
        config.workers.push(WorkerConfig {
            id: "paper-execution".into(),
            role: WorkerRole::Execution,
            enabled: true,
            account_id: Some("main".into()),
            venue_id: Some("paper".into()),
            endpoint: None,
            symbols: Vec::new(),
            settlement_currency: Some("USDT".into()),
            credential_env: None,
            credential_files: None,
            instrument_spec_path: None,
            paper_initial_cash_raw: Some(100_000_000_000_000),
            max_order_notional_raw: None,
            max_position_notional_raw: None,
        });
        assert!(config.validate().is_err());
        config.workers.last_mut().unwrap().instrument_spec_path = Some("market-spec.json".into());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn execution_risk_limits_require_a_frozen_instrument_spec() {
""",
)

risk = "crates/qx-zhenlu/src/lib.rs"
replace_once(
    risk,
    """        let Some(spec) = self.instrument_spec.as_ref() else {
            if self.available_margin_raw.is_some()
                || self.max_order_notional_raw.is_some()
                || self.max_position_notional_raw.is_some()
            {
                return Err(QxError::BusinessViolation(
                    "账户级 RiskContext 缺少 TradingInstrumentSpec".into(),
                ));
            }
            return Ok(());
        };
""",
    """        let Some(spec) = self.instrument_spec.as_ref() else {
            return Err(QxError::BusinessViolation(
                "账户级 RiskContext 缺少 TradingInstrumentSpec".into(),
            ));
        };
""",
)

replace_once(
    risk,
    """    #[test]
    fn risk_context_enforces_spec_leverage_and_available_margin() {
""",
    """    #[test]
    fn risk_context_without_instrument_spec_is_always_rejected() {
        let candidate = order(1, Side::Buy);
        assert!(RiskContext::default()
            .validate_order(&candidate, &PositionSnapshot::default())
            .is_err());
    }

    #[test]
    fn risk_context_enforces_spec_leverage_and_available_margin() {
""",
)
