//! 回测编排：策略回测、内置策略回测、双腿回测、批量 manifest 与结果产物落盘。

use super::*;

struct SmaBarStrategy {
    fast: usize,
    slow: usize,
}

impl BarStrategy for SmaBarStrategy {
    fn on_bar(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        position: i128,
    ) -> Option<Order> {
        if self.fast == 0 || self.slow == 0 || self.fast >= self.slow {
            return None;
        }
        if history.len() < self.slow + 1 {
            return None;
        }
        let end = history.len();
        let sum = |start: usize, stop: usize| {
            history[start..stop]
                .iter()
                .map(|bar| bar.close)
                .sum::<i128>()
        };
        let fast_now = sum(end - self.fast, end) / self.fast as i128;
        let slow_now = sum(end - self.slow, end) / self.slow as i128;
        let fast_prev = sum(end - self.fast - 1, end - 1) / self.fast as i128;
        let slow_prev = sum(end - self.slow - 1, end - 1) / self.slow as i128;
        let golden = fast_prev <= slow_prev && fast_now > slow_now;
        let death = fast_prev >= slow_prev && fast_now < slow_now;
        let (side, should_trade) = if golden && position == 0 {
            (Side::Buy, true)
        } else if death && position > 0 {
            (Side::Sell, true)
        } else {
            (Side::Buy, false)
        };
        if !should_trade {
            return None;
        }
        Some(Order {
            client_id: 0,
            instrument: instrument.clone(),
            side,
            qty: Quantity::from_i64(1),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        })
    }
}

pub(crate) fn run_strategy_backtest(
    runtime_path: &Path,
    frame_path: &Path,
    spec_path: Option<&Path>,
) -> Result<(), String> {
    let config = read_runtime_config(runtime_path)?;
    let payload = std::fs::read_to_string(frame_path).map_err(|error| {
        format!(
            "读取策略回测 BarFrame 失败 {}: {error}",
            frame_path.display()
        )
    })?;
    let frame = BarFrame::from_json(&payload).map_err(|error| {
        format!(
            "策略回测 BarFrame 校验失败 {}: {error:?}",
            frame_path.display()
        )
    })?;
    let bars: Vec<Bar> = (&frame).into();
    if bars.len() < 2 {
        return Err("跨语言 Bar 回测至少需要两根 Bar".into());
    }
    let provider =
        JsonBarFrameProvider::new(frame.source.0.clone(), "barframe-json-v1", frame_path);
    let (provider_bars, manifest) = provider.load_bars_with_manifest(
        &format!("strategy-bars:{}", frame.instrument),
        &frame.instrument.to_string(),
        bars.first().map(|bar| bar.ts).unwrap_or(1),
        bars.last().map(|bar| bar.ts).unwrap_or(1),
    )?;
    if provider_bars.len() != bars.len()
        || provider_bars.iter().zip(&bars).any(|(left, right)| {
            left.timestamp != right.ts
                || left.open_raw != right.open
                || left.high_raw != right.high
                || left.low_raw != right.low
                || left.close_raw != right.close
                || left.volume_raw != right.volume
        })
    {
        return Err("qx-data Provider 与 BarFrame 列式输入不一致，拒绝开始回测".into());
    }
    let data_root = resolve_runtime_relative_path(runtime_path, &config.storage.data_dir);
    let mut dataset_registry = JsonDatasetRegistry::open(data_root.join("datasets.manifest.json"))?;
    dataset_registry.register(manifest.clone())?;
    dataset_registry.verify(
        &manifest.dataset_id,
        &manifest.version,
        &manifest.fingerprint,
    )?;
    println!(
        "[Data · Dataset] dataset={} version={} source={} fingerprint={}",
        manifest.dataset_id, manifest.version, manifest.source, manifest.fingerprint
    );
    let strategies = if config.strategies.is_empty() {
        vec![config.strategy.clone()]
    } else {
        config.strategies.clone()
    };
    for strategy in strategies {
        let strategy_id = strategy
            .id
            .clone()
            .unwrap_or_else(|| strategy.version.clone());
        let mut strategy_config = config.clone();
        strategy_config.strategy = strategy;
        strategy_config.strategies.clear();
        resolve_strategy_runtime_paths(&mut strategy_config.strategy, runtime_path);
        let (bundle_fingerprint, bundle_components) =
            if let Some(bundle_path) = strategy_config.strategy.dataset_bundle_path.as_deref() {
                let bundle_payload = std::fs::read_to_string(bundle_path).map_err(|error| {
                    format!(
                        "读取策略 DatasetBundleManifest 组件失败 {}: {error}",
                        bundle_path
                    )
                })?;
                let bundle: qx_data::DatasetBundleManifest = serde_json::from_str(&bundle_payload)
                    .map_err(|error| format!("策略 DatasetBundleManifest JSON 无效: {error}"))?;
                let fingerprint =
                    verify_dataset_bundle_binding(Path::new(bundle_path), &manifest, bars.len())?;
                verify_dataset_bundle_component_bindings(
                    &bundle,
                    &strategy_config.strategy,
                    &strategy_id,
                )?;
                let components = bundle
                    .components
                    .iter()
                    .map(|(kind, component)| (kind.clone(), component.dataset.fingerprint.clone()))
                    .collect::<BTreeMap<_, _>>();
                (Some(fingerprint), Some(components))
            } else {
                (None, None)
            };
        if let Some(bundle_path) = strategy_config.strategy.dataset_bundle_path.as_deref() {
            let bundle_payload = std::fs::read_to_string(bundle_path).map_err(|error| {
                format!(
                    "读取策略 DatasetBundleManifest 组件失败 {}: {error}",
                    bundle_path
                )
            })?;
            let bundle: qx_data::DatasetBundleManifest = serde_json::from_str(&bundle_payload)
                .map_err(|error| format!("策略 DatasetBundleManifest JSON 无效: {error}"))?;
            if bundle.component("corporate_actions").is_some()
                && !strategy_config
                    .strategy
                    .dataset_component_paths
                    .contains_key("corporate_actions")
                && strategy_config.strategy.ashare_actions_path.is_none()
            {
                return Err(format!(
                    "策略 Bundle 包含 corporate_actions，但未配置 ashare_actions_path: {strategy_id}"
                ));
            }
            if bundle.component("calendar").is_some()
                && !strategy_config
                    .strategy
                    .dataset_component_paths
                    .contains_key("calendar")
                && strategy_config.strategy.ashare_calendar_path.is_none()
            {
                return Err(format!(
                    "策略 Bundle 包含 calendar，但未配置 ashare_calendar_path: {strategy_id}"
                ));
            }
        }
        verify_strategy_artifact(&strategy_config.strategy)?;
        run_single_strategy_backtest(
            &strategy_config,
            &frame,
            &bars,
            spec_path,
            &strategy_id,
            bundle_fingerprint
                .as_deref()
                .zip(bundle_components.as_ref())
                .map(
                    |(bundle_fingerprint, component_fingerprints)| DatasetRunBinding {
                        bundle_fingerprint,
                        component_fingerprints,
                    },
                ),
            &data_root,
        )?;
    }
    Ok(())
}

fn persist_backtest_run_manifest(root: &Path, manifest: &RunManifest) -> Result<PathBuf, String> {
    let runs_root = root.join("runs");
    std::fs::create_dir_all(&runs_root)
        .map_err(|error| format!("创建回测 RunManifest 目录失败: {error}"))?;
    let safe_run_id = manifest
        .run_id
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.') {
                value
            } else {
                '_'
            }
        })
        .collect::<String>();
    let path = runs_root.join(format!("{safe_run_id}-{:016x}.run.json", manifest.digest()));
    let payload = manifest.to_json()?;
    if path.exists() {
        let existing = std::fs::read_to_string(&path).map_err(|error| {
            format!("读取已有回测 RunManifest 失败 {}: {error}", path.display())
        })?;
        let restored = RunManifest::from_json(&existing)?;
        if restored != *manifest {
            return Err(format!(
                "同一回测 RunManifest 路径已存在不同内容: {}",
                path.display()
            ));
        }
        return Ok(path);
    }
    let temporary = path.with_extension(format!("run.json.tmp.{}", std::process::id()));
    std::fs::write(&temporary, payload)
        .map_err(|error| format!("写入回测 RunManifest 失败 {}: {error}", temporary.display()))?;
    if let Err(error) = std::fs::rename(&temporary, &path) {
        let _ = std::fs::remove_file(&temporary);
        if !path.exists() {
            return Err(format!(
                "提交回测 RunManifest 失败 {}: {error}",
                path.display()
            ));
        }
    }
    Ok(path)
}

fn write_backtest_artifact(path: &Path, payload: &str, label: &str) -> Result<(), String> {
    if path.exists() {
        let existing = std::fs::read_to_string(path)
            .map_err(|error| format!("读取已有{label}失败 {}: {error}", path.display()))?;
        if existing == payload {
            return Ok(());
        }
        return Err(format!(
            "同一回测{label}路径已存在不同内容: {}",
            path.display()
        ));
    }
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    std::fs::write(&temporary, payload)
        .map_err(|error| format!("写入{label}失败 {}: {error}", temporary.display()))?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        if path.exists() {
            let existing = std::fs::read_to_string(path).map_err(|read_error| {
                format!("读取并发生成的{label}失败 {}: {read_error}", path.display())
            })?;
            let _ = std::fs::remove_file(&temporary);
            if existing == payload {
                return Ok(());
            }
        }
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("提交{label}失败 {}: {error}", path.display()));
    }
    Ok(())
}

fn persist_backtest_artifacts(
    manifest_path: &Path,
    strategy_id: &str,
    instrument: &InstrumentId,
    bars: &[Bar],
    report: &qx_xingban::BacktestReport,
) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let stem = manifest_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("backtest.run.json")
        .strip_suffix(".run.json")
        .unwrap_or("backtest");
    let root = manifest_path
        .parent()
        .ok_or_else(|| "回测 RunManifest 缺少父目录".to_string())?;
    let summary_path = root.join(format!("{stem}.summary.json"));
    let equity_path = root.join(format!("{stem}.equity.csv"));
    let fills_path = root.join(format!("{stem}.fills.csv"));
    let summary = serde_json::json!({
        "schema_version": 1,
        "strategy_id": strategy_id,
        "instrument": instrument.to_string(),
        "bars": bars.len(),
        "fills": report.fills.len(),
        "clock_start": report.clock_start,
        "clock_end": report.clock_end,
        "input_data_hash": format!("{:016x}", report.input_data_hash),
        "result_hash": format!("{:016x}", report.result_hash()),
        "replay_hash": format!("{:016x}", report.replay_hash()),
        "metrics": {
            "return_bps": report.return_bps,
            "max_drawdown_bps": report.max_drawdown_bps,
            "fees_raw": report.fees_raw,
            "turnover_raw": report.turnover_raw,
            "final_equity_raw": report.final_equity(),
        },
        "assumptions": &report.assumptions,
        "model_descriptors": &report.model_descriptors,
        "run_manifest": manifest_path.to_string_lossy(),
    });
    let summary_payload = serde_json::to_string_pretty(&summary)
        .map_err(|error| format!("编码回测摘要失败: {error}"))?;
    write_backtest_artifact(&summary_path, &summary_payload, "回测摘要")?;

    let mut equity_payload = String::from("index,ts,equity_raw,position_raw\n");
    for (index, equity) in report.equity.iter().enumerate() {
        let ts = bars.get(index).map(|bar| bar.ts).unwrap_or_default();
        let position = report.positions.get(index).copied().unwrap_or_default();
        equity_payload.push_str(&format!("{index},{ts},{equity},{position}\n"));
    }
    write_backtest_artifact(&equity_path, &equity_payload, "权益曲线")?;

    let mut fills_payload = String::from(
        "order_id,ts,qty_raw,price_raw,fee_raw,account_id,strategy_id,signal_id,intent_id,venue_id,venue_order_id\n",
    );
    for fill in &report.fills {
        fills_payload.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{}\n",
            fill.order_id,
            fill.ts,
            fill.qty.raw(),
            fill.price.raw(),
            fill.fee.raw(),
            fill.account_id,
            fill.strategy_id.as_deref().unwrap_or_default(),
            fill.signal_id
                .map(|value| value.to_string())
                .unwrap_or_default(),
            fill.intent_id
                .map(|value| value.to_string())
                .unwrap_or_default(),
            fill.venue_id.as_deref().unwrap_or_default(),
            fill.venue_order_id.as_deref().unwrap_or_default(),
        ));
    }
    write_backtest_artifact(&fills_path, &fills_payload, "成交明细")?;
    Ok((summary_path, equity_path, fills_path))
}

pub(crate) fn run_fast_backtest_manifest(manifest_path: &Path) -> Result<(), String> {
    let payload = std::fs::read_to_string(manifest_path).map_err(|error| {
        format!(
            "读取快速回测 manifest 失败 {}: {error}",
            manifest_path.display()
        )
    })?;
    let document: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|error| format!("快速回测 manifest JSON 无效: {error}"))?;
    let jobs = document
        .get("jobs")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "快速回测 manifest 必须包含 jobs 数组".to_string())?;
    if jobs.is_empty() || jobs.len() > 256 {
        return Err("快速回测 jobs 数量必须在 1..=256 内".into());
    }
    let base = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let resolve = |value: &str| {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            base.join(path)
        }
    };
    let mut parsed = Vec::with_capacity(jobs.len());
    for (index, job) in jobs.iter().enumerate() {
        let object = job
            .as_object()
            .ok_or_else(|| format!("快速回测 jobs[{index}] 必须是对象"))?;
        let runtime = object
            .get("runtime")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("快速回测 jobs[{index}] 缺少 runtime"))?;
        let bars = object
            .get("bars")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("快速回测 jobs[{index}] 缺少 bars"))?;
        let spec = object
            .get("market_spec")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(resolve);
        parsed.push((index, resolve(runtime), resolve(bars), spec));
    }
    let results = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(parsed.len());
        for (index, runtime, bars, spec) in parsed {
            handles.push(scope.spawn(move || {
                run_strategy_backtest(&runtime, &bars, spec.as_deref())
                    .map(|_| index)
                    .map_err(|error| format!("jobs[{index}] {error}"))
            }));
        }
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| "快速回测任务线程 panic".to_string())?
            })
            .collect::<Result<Vec<_>, String>>()
    })?;
    println!(
        "[Fast Backtest] manifest={} jobs={} completed={}",
        manifest_path.display(),
        jobs.len(),
        results.len()
    );
    Ok(())
}

pub(crate) struct DatasetRunBinding<'a> {
    bundle_fingerprint: &'a str,
    component_fingerprints: &'a BTreeMap<String, String>,
}

pub(crate) fn run_single_strategy_backtest(
    config: &RuntimeConfig,
    frame: &BarFrame,
    bars: &[Bar],
    spec_path: Option<&Path>,
    strategy_id: &str,
    dataset_binding: Option<DatasetRunBinding<'_>>,
    run_manifest_root: &Path,
) -> Result<(), String> {
    let mut margin: Box<dyn MarginRule> = Box::new(NoMargin);
    let instrument_spec = if let Some(spec_path) = spec_path {
        let spec_payload = std::fs::read_to_string(spec_path).map_err(|error| {
            format!(
                "读取策略回测 market spec 失败 {}: {error}",
                spec_path.display()
            )
        })?;
        let market: serde_json::Value = serde_json::from_str(&spec_payload)
            .map_err(|error| format!("策略回测 market spec JSON 无效: {error}"))?;
        margin = ccxt_margin_rule_from_market(&market);
        Some(ccxt_market_to_spec(&frame.instrument, &market)?)
    } else {
        None
    };
    if config
        .strategy
        .product
        .is_some_and(TradingProduct::is_derivative)
        && instrument_spec.is_none()
    {
        return Err("衍生品跨语言回测必须提供 market-spec.json".into());
    }
    let mut virtual_trading = VirtualTradingConfig::default();
    let costs = load_cost_rules(config.strategy.cost_rules_path.as_deref().map(Path::new))?;
    let fee: Box<dyn FeeModel> = if let Some(path) = config.strategy.ashare_rules_path.as_deref() {
        let payload = std::fs::read_to_string(path)
            .map_err(|error| format!("读取 A 股规则快照失败 {path}: {error}"))?;
        let mut rules: AshareRuleConfig = serde_json::from_str(&payload)
            .map_err(|error| format!("A 股规则快照 JSON 无效 {path}: {error}"))?;
        if let Some(actions_path) = config.strategy.ashare_actions_path.as_deref() {
            let actions_payload = std::fs::read_to_string(actions_path)
                .map_err(|error| format!("读取 A 股公司行为 JSON 失败 {actions_path}: {error}"))?;
            rules
                .apply_corporate_actions_json(&frame.instrument.to_string(), &actions_payload)
                .map_err(|error| format!("A 股公司行为 JSON 非法: {error}"))?;
        }
        if let Some(calendar_path) = config.strategy.ashare_calendar_path.as_deref() {
            let calendar_payload = std::fs::read_to_string(calendar_path)
                .map_err(|error| format!("读取 A 股交易日历 JSON 失败 {calendar_path}: {error}"))?;
            rules
                .apply_calendar_json(&calendar_payload)
                .map_err(|error| format!("A 股交易日历 JSON 非法: {error}"))?;
        }
        rules
            .validate()
            .map_err(|error| format!("A 股规则快照非法: {error}"))?;
        if !rules.enabled {
            return Err("配置 ashare_rules_path 后 enabled 必须为 true".into());
        }
        virtual_trading.ashare_rules = Some(rules.clone());
        Box::new(AShareFeeModel {
            commission_bp: rules.commission_bp,
            min_commission: rules.min_commission,
            stamp_duty_bp: rules.stamp_duty_bp,
            transfer_fee_bp: rules.transfer_fee_bp,
        })
    } else {
        costs.fee_model()
    };
    if (config.strategy.ashare_actions_path.is_some()
        || config.strategy.ashare_calendar_path.is_some())
        && config.strategy.ashare_rules_path.is_none()
    {
        return Err(
            "配置 ashare_actions_path 或 ashare_calendar_path 时必须同时配置 ashare_rules_path"
                .into(),
        );
    }
    let account_id = config
        .strategy
        .account_id
        .clone()
        .unwrap_or_else(|| "backtest".into());
    let currency = instrument_spec
        .as_ref()
        .map(|spec| spec.settlement_currency.clone())
        .unwrap_or_else(|| "USDT".into());
    let initial_cash = Money::from_i64(100_000);
    let backtest_config = BacktestConfig {
        instrument: frame.instrument.clone(),
        instrument_spec,
        account_id,
        currency,
        initial_cash,
        multiplier: 1,
        fill: Box::new(NextBarOpenFillModel),
        fee,
        data_tier: DataTier::Bar,
        latency: costs.latency_model(),
        margin,
        seed: 20260911,
        risk: RiskGate::new(),
        virtual_trading,
    };
    let report = if config.strategy.builtin_strategy.is_some() {
        if let Some(configured) = config.strategy.instrument.as_deref() {
            let configured = InstrumentId::parse(configured)
                .ok_or_else(|| format!("Strategy instrument 非法: {configured}"))?;
            if configured != frame.instrument {
                return Err(format!(
                    "内置策略回测 instrument 不一致: strategy={} frame={}",
                    configured, frame.instrument
                ));
            }
        }
        let builtin_config =
            builtin_strategy_config_from_runtime(&config.strategy, &frame.instrument)?;
        let context = NativeStrategyContext {
            strategy_id: builtin_config.strategy_id.clone(),
            strategy_version: builtin_config.strategy_version.clone(),
            account_id: config
                .strategy
                .account_id
                .clone()
                .unwrap_or_else(|| "backtest".into()),
            venue_id: config
                .strategy
                .venue_id
                .clone()
                .unwrap_or_else(|| frame.instrument.venue.to_string()),
            data_fingerprint: dataset_binding
                .as_ref()
                .map(|binding| format!("dataset-bundle:{}", binding.bundle_fingerprint))
                .unwrap_or_else(|| format!("barframe:{:016x}", frame.digest())),
            as_of: bars.first().map(|bar| bar.ts).unwrap_or(1),
            positions: BTreeMap::new(),
            cash: BTreeMap::from([(backtest_config.currency.clone(), initial_cash.raw())]),
            available_margin_raw: Some(initial_cash.raw()),
            risk_state: "backtest-verified".into(),
        };
        let strategy = BuiltinStrategy::new(builtin_config)?;
        let mut strategy = NativeBarStrategy::new(strategy, context);
        strategy
            .initialize()
            .map_err(|error| format!("初始化内置策略失败: {error:?}"))?;
        BacktestEngine::new(backtest_config)
            .run(bars, &mut strategy)
            .map_err(|error| format!("内置策略回测失败: {error:?}"))?
    } else {
        let mut strategy = ContractBarStrategy::from_config(
            config.clone(),
            frame,
            initial_cash,
            backtest_config.currency.clone(),
            dataset_binding
                .as_ref()
                .map(|binding| binding.bundle_fingerprint),
        )?;
        BacktestEngine::new(backtest_config)
            .run(bars, &mut strategy)
            .map_err(|error| format!("跨语言策略回测失败: {error:?}"))?
    };
    let data_fingerprint = dataset_binding
        .as_ref()
        .map(|binding| format!("dataset-bundle:{}", binding.bundle_fingerprint))
        .unwrap_or_else(|| format!("{:016x}", report.input_data_hash));
    let run_manifest = report.run_manifest_with_input_components(
        RunManifestIdentity {
            run_id: &format!("strategy-backtest:{strategy_id}:{}", frame.instrument),
            code_commit: "workspace",
            config_hash: &config.fingerprint()?,
            strategy_version: &config.strategy.version,
            instrument_spec_version: if spec_path.is_some() {
                "ccxt-market-spec-v1"
            } else {
                "default-instrument-spec-v1"
            },
            runtime_version: &format!("runtime-schema-{}", config.schema_version),
        },
        &data_fingerprint,
        dataset_binding
            .map(|binding| binding.component_fingerprints.clone())
            .unwrap_or_default(),
    )?;
    let run_manifest_path = persist_backtest_run_manifest(run_manifest_root, &run_manifest)?;
    let (summary_path, equity_path, fills_path) = persist_backtest_artifacts(
        &run_manifest_path,
        strategy_id,
        &frame.instrument,
        bars,
        &report,
    )?;
    println!(
        "[Strategy · Backtest] strategy={} instrument={} bars={} fills={} return_bps={} max_drawdown_bps={} result_hash={:016x}",
        strategy_id,
        frame.instrument,
        bars.len(),
        report.fills.len(),
        report.return_bps,
        report.max_drawdown_bps,
        report.result_hash()
    );
    println!(
        "[RunManifest] run_id={} data_fingerprint={} result_hash={} digest={:016x}",
        run_manifest.run_id,
        run_manifest.data_fingerprint,
        run_manifest.result_hash,
        run_manifest.digest()
    );
    println!("[RunManifest] path={}", run_manifest_path.display());
    println!(
        "[Artifacts] summary={} equity={} fills={}",
        summary_path.display(),
        equity_path.display(),
        fills_path.display()
    );
    Ok(())
}

pub(crate) fn run_ccxt_backtest(
    frame_path: &Path,
    fast: usize,
    slow: usize,
    spec_path: Option<&Path>,
    costs_path: Option<&Path>,
) -> Result<(), String> {
    if fast == 0 || slow == 0 || fast >= slow {
        return Err("回测 fast/slow 必须满足 0 < fast < slow".into());
    }
    let payload = std::fs::read_to_string(frame_path)
        .map_err(|error| format!("读取 BarFrame 失败 {}: {error}", frame_path.display()))?;
    let frame = BarFrame::from_json(&payload)
        .map_err(|error| format!("BarFrame 校验失败 {}: {error:?}", frame_path.display()))?;
    let instrument = frame.instrument.clone();
    let bars: Vec<Bar> = (&frame).into();
    let mut margin: Box<dyn MarginRule> = Box::new(NoMargin);
    let instrument_spec = if let Some(spec_path) = spec_path {
        let spec_payload = std::fs::read_to_string(spec_path).map_err(|error| {
            format!(
                "读取 CCXT market spec 失败 {}: {error}",
                spec_path.display()
            )
        })?;
        let market: serde_json::Value = serde_json::from_str(&spec_payload)
            .map_err(|error| format!("CCXT market spec JSON 无效: {error}"))?;
        margin = ccxt_margin_rule_from_market(&market);
        Some(ccxt_market_to_spec(&instrument, &market)?)
    } else {
        None
    };
    let costs = load_cost_rules(costs_path)?;
    let config = BacktestConfig {
        instrument,
        instrument_spec,
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(100_000),
        multiplier: 1,
        fill: Box::new(NextBarOpenFillModel),
        fee: costs.fee_model(),
        data_tier: DataTier::Bar,
        latency: costs.latency_model(),
        margin,
        seed: 20260911,
        risk: RiskGate::new(),
        virtual_trading: VirtualTradingConfig::default(),
    };
    let report = BacktestEngine::new(config)
        .run(&bars, &mut SmaBarStrategy { fast, slow })
        .map_err(|error| format!("CCXT 快照回测失败: {error:?}"))?;
    println!(
        "[CCXT · Backtest] instrument={} bars={} fills={} return_bps={} max_drawdown_bps={} result_hash={:016x}",
        frame.instrument,
        bars.len(),
        report.fills.len(),
        report.return_bps,
        report.max_drawdown_bps,
        report.result_hash()
    );
    Ok(())
}

pub(crate) fn run_builtin_backtest(
    strategy_name: &str,
    frame_path: &Path,
    spec_path: Option<&Path>,
    quantity: i64,
    costs_path: Option<&Path>,
) -> Result<(), String> {
    if quantity <= 0 {
        return Err("内置策略 quantity 必须为正整数".into());
    }
    let kind = BuiltinStrategyKind::parse(strategy_name)?;
    let payload = std::fs::read_to_string(frame_path).map_err(|error| {
        format!(
            "读取内置策略 BarFrame 失败 {}: {error}",
            frame_path.display()
        )
    })?;
    let frame = BarFrame::from_json(&payload).map_err(|error| {
        format!(
            "内置策略 BarFrame 校验失败 {}: {error:?}",
            frame_path.display()
        )
    })?;
    let bars: Vec<Bar> = (&frame).into();
    if bars.len() < 3 {
        return Err("内置策略回测至少需要三根 Bar".into());
    }

    let mut margin: Box<dyn MarginRule> = Box::new(NoMargin);
    let instrument_spec = if let Some(spec_path) = spec_path {
        let spec_payload = std::fs::read_to_string(spec_path).map_err(|error| {
            format!(
                "读取内置策略 market spec 失败 {}: {error}",
                spec_path.display()
            )
        })?;
        let market: serde_json::Value = serde_json::from_str(&spec_payload)
            .map_err(|error| format!("内置策略 market spec JSON 无效: {error}"))?;
        margin = ccxt_margin_rule_from_market(&market);
        Some(ccxt_market_to_spec(&frame.instrument, &market)?)
    } else {
        None
    };
    let context = NativeStrategyContext {
        strategy_id: format!("builtin-{}", kind.name()),
        strategy_version: format!("builtin-{}-v1", kind.name()),
        account_id: "main".into(),
        venue_id: frame.instrument.venue.to_string(),
        data_fingerprint: format!("barframe:{:?}", frame.source),
        as_of: bars.first().map(|bar| bar.ts).unwrap_or(1),
        positions: BTreeMap::new(),
        cash: BTreeMap::from([("USDT".into(), Money::from_i64(100_000).raw())]),
        available_margin_raw: Some(Money::from_i64(100_000).raw()),
        risk_state: "ready".into(),
    };
    let strategy_config = BuiltinStrategyConfig::new(
        kind,
        format!("builtin-{}", kind.name()),
        frame.instrument.clone(),
        Quantity::from_i64(quantity),
    )?;
    let strategy = BuiltinStrategy::new(strategy_config)?;
    let mut strategy = NativeBarStrategy::new(strategy, context);
    strategy
        .initialize()
        .map_err(|error| format!("初始化内置策略失败: {error:?}"))?;
    let costs = load_cost_rules(costs_path)?;
    let config = BacktestConfig {
        instrument: frame.instrument.clone(),
        instrument_spec,
        account_id: "main".into(),
        currency: "USDT".into(),
        initial_cash: Money::from_i64(100_000),
        multiplier: 1,
        fill: Box::new(NextBarOpenFillModel),
        fee: costs.fee_model(),
        data_tier: DataTier::Bar,
        latency: costs.latency_model(),
        margin,
        seed: 20260914,
        risk: RiskGate::new(),
        virtual_trading: VirtualTradingConfig::default(),
    };
    let report = BacktestEngine::new(config)
        .run(&bars, &mut strategy)
        .map_err(|error| format!("内置策略回测失败: {error:?}"))?;
    println!(
        "[Builtin · Backtest] strategy={} instrument={} bars={} fills={} return_bps={} max_drawdown_bps={} result_hash={:016x}",
        kind.name(),
        frame.instrument,
        bars.len(),
        report.fills.len(),
        report.return_bps,
        report.max_drawdown_bps,
        report.result_hash()
    );
    Ok(())
}

struct ScheduledTargetStrategy {
    instrument: InstrumentId,
    targets: BTreeMap<u64, i128>,
    policy: Option<OrderPolicy>,
    account_id: String,
}

impl BarStrategy for ScheduledTargetStrategy {
    fn on_bar(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        _ts: u64,
        position: i128,
    ) -> Option<Order> {
        if instrument != &self.instrument {
            return None;
        }
        let visible_ts = history.last()?.ts;
        let target = self
            .targets
            .range(..=visible_ts)
            .next_back()
            .map(|(_, target)| *target)
            .unwrap_or(0);
        let delta = target.checked_sub(position)?;
        if delta == 0 {
            return None;
        }
        Some(Order {
            client_id: 0,
            instrument: self.instrument.clone(),
            side: if delta > 0 { Side::Buy } else { Side::Sell },
            qty: Quantity::from_raw(delta.checked_abs()?),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: self.account_id.clone(),
            trace: None,
            policy: self.policy,
        })
    }
}

fn read_bar_frame_for_multi_backtest(path: &Path, label: &str) -> Result<BarFrame, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取{label} BarFrame 失败 {}: {error}", path.display()))?;
    BarFrame::from_json(&payload)
        .map_err(|error| format!("{label} BarFrame 校验失败 {}: {error:?}", path.display()))
}

fn multi_backtest_market_spec(
    instrument: &InstrumentId,
    path: Option<&Path>,
) -> Result<(Option<TradingInstrumentSpec>, Box<dyn MarginRule>), String> {
    let Some(path) = path else {
        return Ok((None, Box::new(NoMargin)));
    };
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取多腿 market spec 失败 {}: {error}", path.display()))?;
    let market: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|error| format!("多腿 market spec JSON 无效 {}: {error}", path.display()))?;
    let margin = ccxt_margin_rule_from_market(&market);
    let spec = ccxt_market_to_spec(instrument, &market)?;
    Ok((Some(spec), margin))
}

pub(crate) fn run_multi_builtin_backtest(
    strategy_name: &str,
    primary_path: &Path,
    reference_path: &Path,
    primary_spec_path: Option<&Path>,
    reference_spec_path: Option<&Path>,
    quantity: i64,
    costs_path: Option<&Path>,
) -> Result<(), String> {
    if quantity <= 0 {
        return Err("多腿内置策略 quantity 必须为正整数".into());
    }
    let kind = BuiltinStrategyKind::parse(strategy_name)?;
    if !matches!(
        kind,
        BuiltinStrategyKind::PairsArbitrage
            | BuiltinStrategyKind::BasisArbitrage
            | BuiltinStrategyKind::CrossVenueArbitrage
            | BuiltinStrategyKind::SpotFuturesArbitrage
    ) {
        return Err(format!("多腿回测只接受套利策略，当前为 {}", kind.name()));
    }
    let primary_frame = read_bar_frame_for_multi_backtest(primary_path, "主腿")?;
    let reference_frame = read_bar_frame_for_multi_backtest(reference_path, "对冲腿")?;
    if primary_frame.ts != reference_frame.ts {
        return Err("多腿回测要求两条 BarFrame 的时间戳完全对齐".into());
    }
    let primary_bars: Vec<Bar> = (&primary_frame).into();
    let reference_bars: Vec<Bar> = (&reference_frame).into();
    if primary_bars.len() < 3 {
        return Err("多腿内置策略回测至少需要三根对齐 Bar".into());
    }
    let strategy_config = BuiltinStrategyConfig {
        kind,
        strategy_id: format!("builtin-{}-multi", kind.name()),
        strategy_version: format!("builtin-{}-v1", kind.name()),
        instrument: primary_frame.instrument.clone(),
        quantity: Quantity::from_i64(quantity),
        fast_window: 5,
        slow_window: 20,
        period: 14,
        threshold_bps: 100,
        reference_instrument: Some(reference_frame.instrument.clone()),
        primary_policy: None,
        reference_policy: None,
    };
    let mut native = BuiltinStrategy::new(strategy_config)?;
    let mut context = NativeStrategyContext {
        strategy_id: format!("builtin-{}-multi", kind.name()),
        strategy_version: format!("builtin-{}-v1", kind.name()),
        account_id: "multi-leg-backtest".into(),
        venue_id: primary_frame.instrument.venue.to_string(),
        data_fingerprint: format!("{:?}:{:?}", primary_frame.source, reference_frame.source),
        as_of: primary_bars.first().map(|bar| bar.ts).unwrap_or(1),
        positions: BTreeMap::from([
            (primary_frame.instrument.to_string(), 0),
            (reference_frame.instrument.to_string(), 0),
        ]),
        cash: BTreeMap::from([("USDT".into(), Money::from_i64(100_000).raw())]),
        available_margin_raw: Some(Money::from_i64(100_000).raw()),
        risk_state: "multi-leg-backtest".into(),
    };
    native
        .on_init(&context)
        .map_err(|error| format!("初始化多腿内置策略失败: {error}"))?;
    let mut primary_targets = BTreeMap::new();
    let mut reference_targets = BTreeMap::new();
    for (primary_bar, reference_bar) in primary_bars.iter().zip(&reference_bars) {
        context.as_of = primary_bar.ts;
        let reference_event = NativeMarketEvent::Bar {
            instrument: reference_frame.instrument.clone(),
            ts: reference_bar.ts,
            open_raw: reference_bar.open,
            high_raw: reference_bar.high,
            low_raw: reference_bar.low,
            close_raw: reference_bar.close,
            volume_raw: reference_bar.volume,
        };
        native
            .on_event(&context, &reference_event)
            .map_err(|error| format!("处理多腿对冲 Bar 失败: {error}"))?;
        let primary_event = NativeMarketEvent::Bar {
            instrument: primary_frame.instrument.clone(),
            ts: primary_bar.ts,
            open_raw: primary_bar.open,
            high_raw: primary_bar.high,
            low_raw: primary_bar.low,
            close_raw: primary_bar.close,
            volume_raw: primary_bar.volume,
        };
        let decision = native
            .on_event(&context, &primary_event)
            .map_err(|error| format!("处理多腿主 Bar 失败: {error}"))?;
        for intent in decision.intents {
            let instrument_key = intent.instrument.to_string();
            let current = context.positions.get(&instrument_key).copied().unwrap_or(0);
            let delta = if intent.side == Side::Buy {
                intent.qty.raw()
            } else {
                -intent.qty.raw()
            };
            let target = current.checked_add(delta).ok_or("多腿回测目标仓位溢出")?;
            context.positions.insert(instrument_key.clone(), target);
            if intent.instrument == primary_frame.instrument {
                primary_targets.insert(primary_bar.ts, target);
            } else if intent.instrument == reference_frame.instrument {
                reference_targets.insert(reference_bar.ts, target);
            }
        }
    }
    let (primary_spec, primary_margin) =
        multi_backtest_market_spec(&primary_frame.instrument, primary_spec_path)?;
    let (reference_spec, reference_margin) =
        multi_backtest_market_spec(&reference_frame.instrument, reference_spec_path)?;
    if primary_spec
        .as_ref()
        .is_some_and(|spec| spec.product.is_derivative())
        && primary_spec_path.is_none()
    {
        return Err("主腿衍生品多腿回测必须提供 market spec".into());
    }
    let costs = load_cost_rules(costs_path)?;
    let run_leg = |frame: &BarFrame,
                   bars: &[Bar],
                   spec: Option<TradingInstrumentSpec>,
                   margin: Box<dyn MarginRule>,
                   targets: BTreeMap<u64, i128>,
                   leg: &str|
     -> Result<qx_xingban::BacktestReport, String> {
        let currency = spec
            .as_ref()
            .map(|value| value.settlement_currency.clone())
            .unwrap_or_else(|| "USDT".into());
        let mut strategy = ScheduledTargetStrategy {
            instrument: frame.instrument.clone(),
            targets,
            policy: None,
            account_id: format!("multi-leg-{leg}"),
        };
        BacktestEngine::new(BacktestConfig {
            instrument: frame.instrument.clone(),
            instrument_spec: spec,
            account_id: format!("multi-leg-{leg}"),
            currency,
            initial_cash: Money::from_i64(100_000),
            multiplier: 1,
            fill: Box::new(NextBarOpenFillModel),
            fee: costs.fee_model(),
            data_tier: DataTier::Bar,
            latency: costs.latency_model(),
            margin,
            seed: 20260914,
            risk: RiskGate::new(),
            virtual_trading: VirtualTradingConfig::default(),
        })
        .run(bars, &mut strategy)
        .map_err(|error| format!("{leg} 多腿回测失败: {error:?}"))
    };
    let primary_report = run_leg(
        &primary_frame,
        &primary_bars,
        primary_spec,
        primary_margin,
        primary_targets,
        "primary",
    )?;
    let reference_report = run_leg(
        &reference_frame,
        &reference_bars,
        reference_spec,
        reference_margin,
        reference_targets,
        "reference",
    )?;
    println!(
        "[Multi-leg · Backtest] strategy={} primary={} fills={} return_bps={} reference={} fills={} return_bps={} combined_return_bps={} result_hashes={:016x}/{:016x}",
        kind.name(),
        primary_frame.instrument,
        primary_report.fills.len(),
        primary_report.return_bps,
        reference_frame.instrument,
        reference_report.fills.len(),
        reference_report.return_bps,
        (i64::from(primary_report.return_bps) + i64::from(reference_report.return_bps)) / 2,
        primary_report.result_hash(),
        reference_report.result_hash()
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_ccxt_builtin_backtest(
    ccxt_config_path: &Path,
    strategy_name: &str,
    instrument: &str,
    timeframe: &str,
    start_ms: u64,
    end_ms: u64,
    spec_path: Option<&Path>,
    quantity: i64,
    costs_path: Option<&Path>,
) -> Result<(), String> {
    if end_ms < start_ms {
        return Err("CCXT 内置策略回测 end_ms 不能早于 start_ms".into());
    }
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
    let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path.to_string_lossy(), None)
        .map_err(|error| format!("启动公共 CCXT Worker 失败: {error}"))?;
    let result = client
        .call(serde_json::json!({
            "op": "fetch_ohlcv",
            "instrument": instrument,
            "timeframe": timeframe,
            "start_ms": start_ms,
            "end_ms": end_ms,
        }))
        .map_err(|error| format!("CCXT OHLCV 查询失败: {error}"))?;
    let frame = result
        .get("frame")
        .ok_or_else(|| "CCXT OHLCV 响应缺少 frame".to_string())?;
    let temp_path = std::env::temp_dir().join(format!(
        "qianxing-ccxt-builtin-{}-{}.json",
        std::process::id(),
        runtime_timestamp_ms()
    ));
    std::fs::write(
        &temp_path,
        serde_json::to_string(frame)
            .map_err(|error| format!("编码 CCXT BarFrame 失败: {error}"))?,
    )
    .map_err(|error| format!("写入临时 CCXT BarFrame 失败: {error}"))?;
    let result = run_builtin_backtest(strategy_name, &temp_path, spec_path, quantity, costs_path);
    let _ = std::fs::remove_file(&temp_path);
    result
}

pub(crate) fn backtest_command(argv: &[String]) {
    let runtime = argv.get(2).cloned().map(PathBuf::from);
    let frame = argv.get(3).cloned().map(PathBuf::from);
    let spec = argv.get(4).cloned().map(PathBuf::from);
    if let Err(error) = run_unified_backtest(runtime.as_deref(), frame.as_deref(), spec.as_deref())
    {
        eprintln!("统一策略回测失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn builtin_backtest_command(argv: &[String]) {
    let args = backtest_positional_args(argv);
    let strategy = match args.get(2).cloned() {
        Some(value) => value,
        None => {
            eprintln!(
                "builtin-backtest 需要 strategy bar-frame.json [market-spec.json] [quantity] [--costs costs.json]"
            );
            std::process::exit(2);
        }
    };
    let frame = match args.get(3).cloned() {
        Some(value) => value,
        None => {
            eprintln!("builtin-backtest 缺少 bar-frame.json");
            std::process::exit(2);
        }
    };
    let spec = args.get(4).map(PathBuf::from);
    let quantity = args
        .get(5)
        .map(|value| value.parse::<i64>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("builtin-backtest quantity 非法");
            std::process::exit(2);
        })
        .unwrap_or(1);
    let costs = costs_flag_value(argv);
    if let Err(error) = run_builtin_backtest(
        &strategy,
        Path::new(&frame),
        spec.as_deref(),
        quantity,
        costs.as_deref(),
    ) {
        eprintln!("内置策略回测失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn ccxt_builtin_backtest_command(argv: &[String]) {
    let args = backtest_positional_args(argv);
    let ccxt_config = match args.get(2).cloned() {
        Some(value) => value,
        None => {
            eprintln!("ccxt-builtin-backtest 需要 ccxt-config strategy instrument start_ms end_ms [timeframe] [market-spec.json] [quantity] [--costs costs.json]");
            std::process::exit(2);
        }
    };
    let strategy = match args.get(3).cloned() {
        Some(value) => value,
        None => {
            eprintln!("ccxt-builtin-backtest 缺少 strategy");
            std::process::exit(2);
        }
    };
    let instrument = match args.get(4).cloned() {
        Some(value) => value,
        None => {
            eprintln!("ccxt-builtin-backtest 缺少 instrument");
            std::process::exit(2);
        }
    };
    let start_ms = match args.get(5).and_then(|value| value.parse().ok()) {
        Some(value) => value,
        None => {
            eprintln!("ccxt-builtin-backtest start_ms 非法");
            std::process::exit(2);
        }
    };
    let end_ms = match args.get(6).and_then(|value| value.parse().ok()) {
        Some(value) => value,
        None => {
            eprintln!("ccxt-builtin-backtest end_ms 非法");
            std::process::exit(2);
        }
    };
    let timeframe = args.get(7).cloned().unwrap_or_else(|| "1h".into());
    let spec = args.get(8).map(PathBuf::from);
    let quantity = args
        .get(9)
        .map(|value| value.parse::<i64>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("ccxt-builtin-backtest quantity 非法");
            std::process::exit(2);
        })
        .unwrap_or(1);
    let costs = costs_flag_value(argv);
    if let Err(error) = run_ccxt_builtin_backtest(
        Path::new(&ccxt_config),
        &strategy,
        &instrument,
        &timeframe,
        start_ms,
        end_ms,
        spec.as_deref(),
        quantity,
        costs.as_deref(),
    ) {
        eprintln!("CCXT 内置策略回测失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn multi_builtin_backtest_command(argv: &[String]) {
    let args = backtest_positional_args(argv);
    let strategy = match args.get(2).cloned() {
        Some(value) => value,
        None => {
            eprintln!(
                "multi-builtin-backtest 需要 strategy primary-bar.json reference-bar.json [primary-spec.json] [reference-spec.json] [quantity] [--costs costs.json]"
            );
            std::process::exit(2);
        }
    };
    let primary = match args.get(3).cloned() {
        Some(value) => value,
        None => {
            eprintln!("multi-builtin-backtest 缺少 primary-bar.json");
            std::process::exit(2);
        }
    };
    let reference = match args.get(4).cloned() {
        Some(value) => value,
        None => {
            eprintln!("multi-builtin-backtest 缺少 reference-bar.json");
            std::process::exit(2);
        }
    };
    let primary_spec = args.get(5).map(PathBuf::from);
    let reference_spec = args.get(6).map(PathBuf::from);
    let quantity = args
        .get(7)
        .map(|value| value.parse::<i64>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("multi-builtin-backtest quantity 非法");
            std::process::exit(2);
        })
        .unwrap_or(1);
    let costs = costs_flag_value(argv);
    if let Err(error) = run_multi_builtin_backtest(
        &strategy,
        Path::new(&primary),
        Path::new(&reference),
        primary_spec.as_deref(),
        reference_spec.as_deref(),
        quantity,
        costs.as_deref(),
    ) {
        eprintln!("多腿内置策略回测失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn ccxt_backtest_command(argv: &[String]) {
    let args = backtest_positional_args(argv);
    let frame = match args.get(2).cloned() {
        Some(frame) => frame,
        None => {
            eprintln!(
                "ccxt-backtest 需要 bar-frame.json [fast] [slow] [market-spec.json] [--costs costs.json]"
            );
            std::process::exit(2);
        }
    };
    let fast = args
        .get(3)
        .map(|value| value.parse::<usize>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("ccxt-backtest fast 非法");
            std::process::exit(2);
        })
        .unwrap_or(5);
    let slow = args
        .get(4)
        .map(|value| value.parse::<usize>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("ccxt-backtest slow 非法");
            std::process::exit(2);
        })
        .unwrap_or(20);
    let spec_path = args.get(5).map(std::path::PathBuf::from);
    let costs_path = costs_flag_value(argv);
    if let Err(error) = run_ccxt_backtest(
        Path::new(&frame),
        fast,
        slow,
        spec_path.as_deref(),
        costs_path.as_deref(),
    ) {
        eprintln!("CCXT 快照回测失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn strategy_backtest_command(argv: &[String]) {
    let runtime = match argv.get(2).cloned() {
        Some(runtime) => runtime,
        None => {
            eprintln!("strategy-backtest 需要 runtime.json bar-frame.json [market-spec.json]");
            std::process::exit(2);
        }
    };
    let frame = match argv.get(3).cloned() {
        Some(frame) => frame,
        None => {
            eprintln!("strategy-backtest 缺少 bar-frame.json");
            std::process::exit(2);
        }
    };
    let spec_path = argv.get(4).cloned().map(std::path::PathBuf::from);
    if let Err(error) =
        run_strategy_backtest(Path::new(&runtime), Path::new(&frame), spec_path.as_deref())
    {
        eprintln!("跨语言策略回测失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn fast_backtest_command(argv: &[String]) {
    let manifest = match argv.get(2).cloned() {
        Some(manifest) => manifest,
        None => {
            eprintln!("fast-backtest 需要 manifest.json");
            std::process::exit(2);
        }
    };
    if let Err(error) = run_fast_backtest_manifest(Path::new(&manifest)) {
        eprintln!("快速批量回测失败: {error}");
        std::process::exit(2);
    }
}
