//! Fail-closed preflight for concrete orders before a live connector is created.

use crate::exchange_gateway::{CapabilityProfileV1, InstrumentRulesV1};
use qx_app::{
    evaluate_account_query_capability_preflight_v1,
    evaluate_cancel_execution_capability_preflight_v1, evaluate_execution_capability_preflight_v1,
    AccountQueryCapabilityPreflightDecisionV1, AccountQueryCapabilityPreflightSpecV1,
    CallerCapability, ExecutionCapabilityPreflightDecisionV1, ExecutionCapabilityPreflightSpecV1,
    ExecutionProfile, RunContext, ACCOUNT_QUERY_PREFLIGHT_SCHEMA_VERSION,
    EXECUTION_CAPABILITY_PREFLIGHT_SCHEMA_VERSION,
};
use qx_control::{CommandKind, ControlCommand};
use qx_core::{
    ExactDecimal, ExecutionCapabilityDecisionFact, ExecutionCapabilityDecisionKind, MarginMode,
    Order, OrderStatus, PositionMode, Quantity, Side, TradingProduct,
    EXECUTION_CAPABILITY_DECISION_SCHEMA_VERSION,
};
use qx_runtime::{LiveEventPipeline, WorkerConfig};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Copy, Debug)]
pub enum ExecutionConnectorProfile<'a> {
    BinanceSpot,
    Ccxt { config_path: &'a Path },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountQueryPurpose {
    Reconcile,
    BalanceProbe,
}

struct ResolvedExecutionCapability {
    spec: ExecutionCapabilityPreflightSpecV1,
    decision: ExecutionCapabilityPreflightDecisionV1,
    catalog_fingerprint: String,
}

/// Resolve a real order against the generated capability and maturity catalogs.
/// The function only reads local configuration and market-rule files; it never reads credential
/// values, starts a process, opens an EventLog, or contacts a venue.
pub fn require_order_execution_capability(
    order: &Order,
    worker: &WorkerConfig,
    runtime_environment: &str,
    runtime_config_path: &Path,
    connector: ExecutionConnectorProfile<'_>,
) -> Result<ExecutionCapabilityPreflightDecisionV1, String> {
    let resolved = resolve_order_execution_capability(
        order,
        worker,
        runtime_environment,
        runtime_config_path,
        "submit",
        connector,
    )?;
    require_resolved_decision(order, worker, &resolved.decision)?;
    Ok(resolved.decision)
}

/// Resolve a live cancellation against a distinct operation declaration. A scope that only
/// permits order submission is insufficient evidence to create a connector for cancel RPCs.
pub fn require_cancel_order_execution_capability(
    order: &Order,
    worker: &WorkerConfig,
    runtime_environment: &str,
    runtime_config_path: &Path,
    connector: ExecutionConnectorProfile<'_>,
) -> Result<ExecutionCapabilityPreflightDecisionV1, String> {
    order
        .validate()
        .map_err(|error| format!("撤单订单无效: {error}"))?;
    if order.account_id != worker.account_id.as_deref().unwrap_or_default() {
        return Err("撤单账户与执行 worker 不一致".into());
    }
    let expected_venue = match connector {
        ExecutionConnectorProfile::BinanceSpot => "BINANCE",
        ExecutionConnectorProfile::Ccxt { .. } => worker.venue_id.as_deref().unwrap_or_default(),
    };
    if !order
        .instrument
        .venue
        .as_str()
        .eq_ignore_ascii_case(expected_venue)
    {
        return Err("撤单交易所与执行 worker 不一致".into());
    }
    let resolved = resolve_order_execution_capability(
        order,
        worker,
        runtime_environment,
        runtime_config_path,
        "cancel",
        connector,
    )?;
    require_resolved_decision(order, worker, &resolved.decision)?;
    Ok(resolved.decision)
}

/// Resolve the private account reads a reconciler will use before credentials are loaded or a
/// Binance/CCXT connector process is started. Optional reads missing from the exact scope are
/// returned as omissions so the caller can skip them rather than probing an undeclared endpoint.
pub fn require_account_query_execution_capability(
    worker: &WorkerConfig,
    runtime_environment: &str,
    purpose: AccountQueryPurpose,
    connector: ExecutionConnectorProfile<'_>,
) -> Result<AccountQueryCapabilityPreflightDecisionV1, String> {
    let profile = match runtime_environment.trim().to_ascii_lowercase().as_str() {
        "sandbox" | "testnet" => ExecutionProfile::SandboxAcceptance,
        "production" => ExecutionProfile::Production,
        "paper" => return Err("PAPER 环境不能路由到真实账户私有查询连接器".into()),
        value => return Err(format!("不支持真实账户查询环境: {value}")),
    };
    let (capability_id, venue, product, account_model, sandbox_endpoint) = match connector {
        ExecutionConnectorProfile::BinanceSpot => {
            let venue = worker.venue_id.as_deref().unwrap_or_default();
            if !venue.to_ascii_lowercase().starts_with("binance") {
                return Err(format!("Binance reconciler 的 venue_id 不匹配: {venue}"));
            }
            (
                "qx.venue.binance_spot",
                "BINANCE".to_owned(),
                "spot".to_owned(),
                "spot".to_owned(),
                worker_targets_binance_testnet(worker),
            )
        }
        ExecutionConnectorProfile::Ccxt { config_path } => {
            let venue = worker
                .venue_id
                .as_deref()
                .ok_or_else(|| format!("worker {} 缺少 venue_id", worker.id))?;
            let config = read_ccxt_execution_config(config_path, worker, venue)?;
            let default_type = config
                .get("default_type")
                .and_then(Value::as_str)
                .ok_or_else(|| "CCXT 执行配置必须显式声明 default_type".to_owned())?;
            let product = match default_type.trim().to_ascii_lowercase().as_str() {
                "spot" => TradingProduct::Spot,
                "future" | "swap" => TradingProduct::Perpetual,
                value => return Err(format!("CCXT 私有查询不支持 default_type={value}")),
            };
            let account_model = declared_ccxt_account_model(&config, venue, product)?;
            let sandbox = config
                .get("sandbox")
                .and_then(Value::as_bool)
                .ok_or_else(|| "CCXT 执行配置必须显式声明布尔 sandbox".to_owned())?;
            (
                "qx.venue.ccxt_process",
                venue.to_ascii_uppercase(),
                product_name(product).to_owned(),
                account_model,
                sandbox,
            )
        }
    };
    let is_derivative = product == "perpetual" || product == "future";
    let (required_operations, optional_operations) = match purpose {
        AccountQueryPurpose::BalanceProbe => (vec!["balance.read".to_owned()], Vec::new()),
        AccountQueryPurpose::Reconcile => {
            let mut required = vec!["balance.read".to_owned(), "orders.read".to_owned()];
            if is_derivative {
                required.push("positions.read".to_owned());
            }
            let optional = match connector {
                ExecutionConnectorProfile::BinanceSpot => Vec::new(),
                ExecutionConnectorProfile::Ccxt { .. } if is_derivative => {
                    vec!["cashflows.read".to_owned(), "funding.read".to_owned()]
                }
                ExecutionConnectorProfile::Ccxt { .. } => vec!["cashflows.read".to_owned()],
            };
            (required, optional)
        }
    };
    let spec = AccountQueryCapabilityPreflightSpecV1 {
        schema_version: ACCOUNT_QUERY_PREFLIGHT_SCHEMA_VERSION,
        profile,
        capability_id: capability_id.to_owned(),
        venue,
        product,
        account_model,
        required_operations,
        optional_operations,
        sandbox_endpoint,
    };
    let catalog = qx_api::product_capability_catalog_document()?;
    let context = RunContext::new(
        CallerCapability::Live,
        format!("account-query-preflight-{}", worker.id),
    );
    let decision = evaluate_account_query_capability_preflight_v1(&spec, &catalog, &context)
        .map_err(|error| format!("账户查询能力预检无法完成: {error}"))?;
    decision.require_allowed(&context).map_err(|error| {
        format!(
            "ACCOUNT_QUERY_CAPABILITY_DENIED {} capability={} venue={} product={} model={} code={} reason={} required={:?} omitted_optional={:?}",
            error,
            decision.capability_id,
            decision.venue,
            decision.product,
            decision.account_model,
            decision.reason_code.as_deref().unwrap_or("unknown"),
            decision.reason.as_deref().unwrap_or("未满足账户查询范围或证据门槛"),
            spec.required_operations,
            decision.omitted_optional_operations,
        )
    })?;
    Ok(decision)
}

fn resolve_order_execution_capability(
    order: &Order,
    worker: &WorkerConfig,
    runtime_environment: &str,
    runtime_config_path: &Path,
    operation: &str,
    connector: ExecutionConnectorProfile<'_>,
) -> Result<ResolvedExecutionCapability, String> {
    let profile = match runtime_environment.trim().to_ascii_lowercase().as_str() {
        "sandbox" | "testnet" => ExecutionProfile::SandboxAcceptance,
        "production" => ExecutionProfile::Production,
        "paper" => return Err("PAPER 环境不能路由到真实交易所连接器".into()),
        value => return Err(format!("不支持真实执行环境: {value}")),
    };

    let instrument_rules =
        InstrumentRulesV1::load_for_worker(worker, &order.instrument, runtime_config_path)?;
    let market_spec = instrument_rules.spec;

    let (capability_id, account_model, sandbox_endpoint) = match connector {
        ExecutionConnectorProfile::BinanceSpot => {
            if !order.instrument.venue.is_binance() || market_spec.product != TradingProduct::Spot {
                return Err("BinanceSpotVenue 只允许 Binance 现货规格".into());
            }
            (
                "qx.venue.binance_spot",
                "spot".to_owned(),
                worker_targets_binance_testnet(worker),
            )
        }
        ExecutionConnectorProfile::Ccxt { config_path } => {
            let config = read_ccxt_execution_config(
                config_path,
                worker,
                &order.instrument.venue.to_string(),
            )?;
            let model = declared_ccxt_account_model(
                &config,
                &order.instrument.venue.to_string(),
                market_spec.product,
            )?;
            let sandbox = config
                .get("sandbox")
                .and_then(Value::as_bool)
                .ok_or_else(|| "CCXT 执行配置必须显式声明布尔 sandbox".to_owned())?;
            ("qx.venue.ccxt_process", model, sandbox)
        }
    };

    let (account_mode, margin_mode, position_mode) = match market_spec.product {
        TradingProduct::Spot => (
            "cash".to_owned(),
            "cash".to_owned(),
            "not_applicable".to_owned(),
        ),
        TradingProduct::Perpetual | TradingProduct::Future => {
            let policy = order.policy.ok_or_else(|| {
                "FAIL_CLOSED: 合约订单缺少显式 OrderPolicy，无法判定持仓/保证金模式".to_owned()
            })?;
            let account_mode = position_mode_name(policy.position_mode).to_owned();
            let margin_mode = margin_mode_name(policy.margin_mode).to_owned();
            (account_mode.clone(), margin_mode, account_mode)
        }
        TradingProduct::Margin | TradingProduct::Option => {
            return Err(format!(
                "当前真实执行能力目录未声明 {:?} 产品",
                market_spec.product
            ));
        }
    };

    let order_type = if order.limit_price_decimal().is_some() {
        "limit"
    } else {
        "market"
    };
    let quantity_unit = if order.is_quote_budget_order() {
        "quote"
    } else if market_spec.product.is_derivative() {
        "contracts"
    } else {
        "base"
    };
    let side = match order.side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    };
    let market_rule_fingerprint = instrument_rules.fingerprint;
    let spec = ExecutionCapabilityPreflightSpecV1 {
        schema_version: EXECUTION_CAPABILITY_PREFLIGHT_SCHEMA_VERSION,
        profile,
        capability_id: capability_id.to_owned(),
        venue: order.instrument.venue.to_string().to_ascii_uppercase(),
        product: product_name(market_spec.product).to_owned(),
        account_mode,
        account_model,
        margin_mode,
        position_mode,
        order_type: order_type.to_owned(),
        quantity_unit: quantity_unit.to_owned(),
        side: side.to_owned(),
        market_rule_fingerprint,
        sandbox_endpoint,
    };
    let catalog = qx_api::product_capability_catalog_document()?;
    let capability_profile = CapabilityProfileV1::from_catalog(&catalog, &spec.capability_id)?;
    let context = RunContext::new(
        CallerCapability::Live,
        format!("execution-preflight-{}-{}", worker.id, order.client_id),
    );
    let decision = match operation {
        "submit" => evaluate_execution_capability_preflight_v1(&spec, &catalog, &context),
        "cancel" => evaluate_cancel_execution_capability_preflight_v1(&spec, &catalog, &context),
        other => return Err(format!("不支持的执行能力操作: {other}")),
    }
    .map_err(|error| format!("{operation} 执行能力预检无法完成: {error}"))?;
    let catalog_fingerprint = capability_profile.catalog_fingerprint;
    Ok(ResolvedExecutionCapability {
        spec,
        decision,
        catalog_fingerprint,
    })
}

fn require_resolved_decision(
    order: &Order,
    worker: &WorkerConfig,
    decision: &ExecutionCapabilityPreflightDecisionV1,
) -> Result<(), String> {
    let context = RunContext::new(
        CallerCapability::Live,
        format!("execution-preflight-{}-{}", worker.id, order.client_id),
    );
    decision.require_allowed(&context).map_err(|error| {
        format!(
            "EXECUTION_CAPABILITY_DENIED {} capability={} code={} reason={} refs={:?} market_rules_sha256={}",
            error,
            decision.capability_id,
            decision.reason_code.as_deref().unwrap_or("unknown"),
            decision.reason.as_deref().unwrap_or("能力范围或证据不足"),
            decision.maturity_refs,
            decision.market_rule_fingerprint
        )
    })
}

fn order_fingerprint(order: &Order) -> Result<String, String> {
    // Status and filled quantity evolve after submission. Bind the capability decision to the
    // immutable request terms while retaining exact amount fields and strategy attribution.
    let mut identity = order.clone();
    identity.status = OrderStatus::PendingSubmit;
    identity.filled = Quantity::ZERO;
    if let Some(mut exact) = identity.exact_amounts {
        exact.filled = ExactDecimal::ZERO;
        identity.exact_amounts = Some(exact);
    }
    let bytes = serde_json::to_vec(&identity)
        .map_err(|error| format!("订单能力指纹序列化失败: {error}"))?;
    Ok(qx_strategy::sha256_hex(&bytes))
}

fn preflight_snapshot(resolved: &ResolvedExecutionCapability) -> Result<String, String> {
    serde_json::to_string(&serde_json::json!({
        "schema_version": EXECUTION_CAPABILITY_PREFLIGHT_SCHEMA_VERSION,
        "spec": resolved.spec,
        "decision": resolved.decision,
    }))
    .map_err(|error| format!("执行能力证据序列化失败: {error}"))
}

fn execution_decision_fact(
    order: &Order,
    resolved: &ResolvedExecutionCapability,
    kind: ExecutionCapabilityDecisionKind,
    command_id: Option<u64>,
    order_group_id: Option<String>,
) -> Result<ExecutionCapabilityDecisionFact, String> {
    let order_fingerprint = order_fingerprint(order)?;
    let preflight_json = preflight_snapshot(resolved)?;
    let identity = serde_json::to_vec(&serde_json::json!({
        "schema_version": EXECUTION_CAPABILITY_DECISION_SCHEMA_VERSION,
        "kind": kind,
        "command_id": command_id,
        "order_group_id": order_group_id,
        "client_order_id": order.client_id,
        "account_id": order.account_id,
        "venue_id": resolved.spec.venue,
        "order_fingerprint": order_fingerprint,
        "catalog_fingerprint": resolved.catalog_fingerprint,
        "market_rule_fingerprint": resolved.spec.market_rule_fingerprint,
        "preflight_json": preflight_json,
    }))
    .map_err(|error| format!("执行能力决策身份序列化失败: {error}"))?;
    Ok(ExecutionCapabilityDecisionFact {
        schema_version: EXECUTION_CAPABILITY_DECISION_SCHEMA_VERSION,
        kind,
        decision_id: qx_strategy::sha256_hex(&identity),
        command_id,
        order_group_id,
        client_order_id: order.client_id,
        account_id: order.account_id.clone(),
        venue_id: resolved.spec.venue.clone(),
        order_fingerprint,
        catalog_fingerprint: resolved.catalog_fingerprint.clone(),
        market_rule_fingerprint: resolved.spec.market_rule_fingerprint.clone(),
        preflight_json,
    })
}

fn same_preflight_binding(
    previous: &ExecutionCapabilityDecisionFact,
    current: &ExecutionCapabilityDecisionFact,
) -> bool {
    previous.schema_version == current.schema_version
        && previous.client_order_id == current.client_order_id
        && previous.account_id == current.account_id
        && previous.venue_id.eq_ignore_ascii_case(&current.venue_id)
        && previous.order_fingerprint == current.order_fingerprint
        && previous.market_rule_fingerprint == current.market_rule_fingerprint
        && previous.preflight_json == current.preflight_json
}

/// Evaluate and durably record the exact admitted command before a live connector is created.
pub fn require_and_record_submit_command_execution_capability(
    command: &ControlCommand,
    worker: &WorkerConfig,
    runtime_environment: &str,
    runtime_config_path: &Path,
    connector: ExecutionConnectorProfile<'_>,
    pipeline: &mut LiveEventPipeline,
    now: u64,
) -> Result<ExecutionCapabilityPreflightDecisionV1, String> {
    if command.kind != CommandKind::SubmitOrder {
        return Err("execution capability decision 只接受 SubmitOrder".into());
    }
    let order = qx_control::order_from_submit_command(command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    let expected_account = worker.account_id.as_deref().unwrap_or_default();
    if order.account_id != expected_account {
        return Err("订单账户与执行 worker 不一致".into());
    }
    let expected_venue = match connector {
        ExecutionConnectorProfile::BinanceSpot => "BINANCE",
        ExecutionConnectorProfile::Ccxt { .. } => worker.venue_id.as_deref().unwrap_or_default(),
    };
    if !order
        .instrument
        .venue
        .as_str()
        .eq_ignore_ascii_case(expected_venue)
    {
        return Err("订单交易所与执行 worker 不一致".into());
    }
    let resolved = resolve_order_execution_capability(
        &order,
        worker,
        runtime_environment,
        runtime_config_path,
        "submit",
        connector,
    )?;
    require_resolved_decision(&order, worker, &resolved.decision)?;
    let decision = resolved.decision.clone();
    let fact = execution_decision_fact(
        &order,
        &resolved,
        ExecutionCapabilityDecisionKind::Submit,
        Some(command.command_id),
        command.payload.get("spread_group_id").cloned(),
    )?;
    if let Some(previous) = pipeline.execution_capability_decision(order.client_id) {
        if previous.kind != ExecutionCapabilityDecisionKind::Submit
            || previous.command_id != Some(command.command_id)
            || previous.order_group_id != fact.order_group_id
            || !same_preflight_binding(previous, &fact)
        {
            return Err(format!(
                "client_order_id={} 已绑定其他提交命令或不同执行能力快照，拒绝重放",
                order.client_id
            ));
        }
    } else {
        pipeline
            .record_execution_capability_decision(fact, now)
            .map_err(|error| format!("持久化 SubmitOrder 执行能力证据失败: {error:?}"))?;
    }
    Ok(decision)
}

pub fn require_submit_command_execution_capability(
    command: &ControlCommand,
    worker: &WorkerConfig,
    runtime_environment: &str,
    runtime_config_path: &Path,
    connector: ExecutionConnectorProfile<'_>,
) -> Result<ExecutionCapabilityPreflightDecisionV1, String> {
    let order = qx_control::order_from_submit_command(command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    require_order_execution_capability(
        &order,
        worker,
        runtime_environment,
        runtime_config_path,
        connector,
    )
}

/// Check all locally active orders before a recovery connector is started. If a spread-recovery
/// group exists but no corresponding active order can be found, fail closed rather than opening
/// an un-gated private venue session.
pub fn require_active_orders_execution_capability<'a>(
    orders: impl IntoIterator<Item = &'a Order>,
    worker: &WorkerConfig,
    runtime_environment: &str,
    runtime_config_path: &Path,
    connector: ExecutionConnectorProfile<'_>,
    require_at_least_one: bool,
    pipeline: &mut LiveEventPipeline,
    now: u64,
) -> Result<(), String> {
    let expected_account = worker.account_id.as_deref().unwrap_or_default();
    let expected_venue = match connector {
        ExecutionConnectorProfile::BinanceSpot => "BINANCE",
        ExecutionConnectorProfile::Ccxt { .. } => worker.venue_id.as_deref().unwrap_or_default(),
    };
    let active = orders
        .into_iter()
        .filter(|order| {
            order.account_id == expected_account
                && order
                    .instrument
                    .venue
                    .as_str()
                    .eq_ignore_ascii_case(expected_venue)
                && !order.status.is_terminal()
        })
        .collect::<Vec<_>>();
    if active.is_empty() && require_at_least_one {
        return Err(format!(
            "FAIL_CLOSED: worker {} 有待恢复订单组，但 EventLog 中没有可预检的活动订单",
            worker.id
        ));
    }
    for order in active {
        let resolved = resolve_order_execution_capability(
            order,
            worker,
            runtime_environment,
            runtime_config_path,
            "submit",
            connector,
        )
        .map_err(|error| format!("活动订单 {} 的恢复能力预检失败: {error}", order.client_id))?;
        require_resolved_decision(order, worker, &resolved.decision)
            .map_err(|error| format!("活动订单 {} 的恢复能力预检失败: {error}", order.client_id))?;
        let current = execution_decision_fact(
            order,
            &resolved,
            ExecutionCapabilityDecisionKind::RecoveryAdoption,
            None,
            None,
        )?;
        if let Some(previous) = pipeline.execution_capability_decision(order.client_id) {
            if !same_preflight_binding(previous, &current) {
                return Err(format!(
                    "活动订单 {} 的能力证据与当前订单/市场规则/成熟度快照不同，拒绝自动恢复",
                    order.client_id
                ));
            }
        } else {
            pipeline
                .record_execution_capability_decision(current, now)
                .map_err(|error| {
                    format!(
                        "活动订单 {} 建立恢复接管证据失败: {error:?}",
                        order.client_id
                    )
                })?;
        }
    }
    Ok(())
}

fn read_ccxt_execution_config(
    path: &Path,
    worker: &WorkerConfig,
    expected_exchange: &str,
) -> Result<Value, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取 CCXT 执行配置失败 {}: {error}", path.display()))?;
    let value: Value = serde_json::from_str(&payload)
        .map_err(|error| format!("解析 CCXT 执行配置失败 {}: {error}", path.display()))?;
    let configured = value
        .get("exchange_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "CCXT 执行配置缺少 exchange_id".to_owned())?;
    if !configured.eq_ignore_ascii_case(expected_exchange) {
        return Err(format!(
            "CCXT exchange_id={} 与订单 Venue={} 不一致 (worker {})",
            configured, expected_exchange, worker.id
        ));
    }
    Ok(value)
}

fn declared_ccxt_account_model(
    config: &Value,
    venue: &str,
    product: TradingProduct,
) -> Result<String, String> {
    let model = config
        .get("account_model")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "CCXT 执行配置必须显式声明 account_model".to_owned())?;
    let default_type = config
        .get("default_type")
        .and_then(Value::as_str)
        .ok_or_else(|| "CCXT 执行配置必须显式声明 default_type".to_owned())?;
    let expected_type_matches = match product {
        TradingProduct::Spot => default_type.eq_ignore_ascii_case("spot"),
        TradingProduct::Perpetual => match venue.to_ascii_lowercase().as_str() {
            "binance" => matches!(
                default_type.to_ascii_lowercase().as_str(),
                "future" | "swap"
            ),
            "okx" | "bitget" => default_type.eq_ignore_ascii_case("swap"),
            _ => false,
        },
        _ => false,
    };
    if !expected_type_matches {
        return Err(format!(
            "CCXT default_type={} 与冻结产品 {:?} 不一致",
            default_type, product
        ));
    }
    let model = model.trim().to_ascii_lowercase();
    let model_matches = match venue.to_ascii_lowercase().as_str() {
        "binance" if product == TradingProduct::Spot => model == "spot",
        "binance" if product.is_derivative() => model == "usd_m_futures",
        "okx" => model == "unified",
        "bitget" => {
            let uta = config
                .get("options")
                .and_then(|options| options.get("uta"))
                .and_then(Value::as_bool)
                .ok_or_else(|| "Bitget CCXT 执行配置必须显式声明 options.uta".to_owned())?;
            (uta && model == "uta") || (!uta && model == "classic")
        }
        _ => false,
    };
    if !model_matches {
        return Err(format!(
            "CCXT account_model={} 与 venue/product/config 不一致",
            model
        ));
    }
    Ok(model)
}

fn worker_targets_binance_testnet(worker: &WorkerConfig) -> bool {
    worker
        .venue_id
        .as_deref()
        .is_some_and(|venue| venue.to_ascii_lowercase().contains("testnet"))
        || worker
            .endpoint
            .as_deref()
            .is_some_and(|endpoint| endpoint.to_ascii_lowercase().contains("testnet"))
}

fn product_name(product: TradingProduct) -> &'static str {
    match product {
        TradingProduct::Spot => "spot",
        TradingProduct::Perpetual => "perpetual",
        TradingProduct::Future => "future",
        TradingProduct::Margin => "margin",
        TradingProduct::Option => "option",
    }
}

fn margin_mode_name(mode: MarginMode) -> &'static str {
    match mode {
        MarginMode::Cash => "cash",
        MarginMode::Cross => "cross",
        MarginMode::Isolated => "isolated",
    }
}

fn position_mode_name(mode: PositionMode) -> &'static str {
    match mode {
        PositionMode::OneWay => "one_way",
        PositionMode::Hedge => "hedge",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_app::{
        evaluate_execution_capability_preflight_v1, ExecutionCapabilityPreflightSpecV1,
        EXECUTION_CAPABILITY_PREFLIGHT_SCHEMA_VERSION,
    };
    use qx_core::{InstrumentId, OrderStatus, Price, Quantity, Side, VenueId};
    use serde_json::json;

    fn test_order() -> Order {
        Order {
            client_id: 41,
            instrument: InstrumentId::new("BTCUSDT", VenueId::new("BINANCE")),
            side: Side::Buy,
            qty: Quantity::from_i64(2),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        }
    }

    fn test_resolved() -> ResolvedExecutionCapability {
        let spec = ExecutionCapabilityPreflightSpecV1 {
            schema_version: EXECUTION_CAPABILITY_PREFLIGHT_SCHEMA_VERSION,
            profile: ExecutionProfile::SandboxAcceptance,
            capability_id: "qx.venue.binance_spot".into(),
            venue: "BINANCE".into(),
            product: "spot".into(),
            account_mode: "cash".into(),
            account_model: "spot".into(),
            margin_mode: "cash".into(),
            position_mode: "not_applicable".into(),
            order_type: "limit".into(),
            quantity_unit: "base".into(),
            side: "buy".into(),
            market_rule_fingerprint: "b".repeat(64),
            sandbox_endpoint: true,
        };
        ResolvedExecutionCapability {
            decision: ExecutionCapabilityPreflightDecisionV1 {
                schema_version: EXECUTION_CAPABILITY_PREFLIGHT_SCHEMA_VERSION,
                allowed: true,
                profile: ExecutionProfile::SandboxAcceptance,
                capability_id: spec.capability_id.clone(),
                scope_status: Some("implemented-unverified".into()),
                maturity_refs: vec!["binance_direct".into()],
                evidence_stages: json!({"binance_direct": {"implementation": true, "code_tested": true}}),
                evidence: vec!["local-contract".into()],
                limitations: vec!["sandbox acceptance pending".into()],
                market_rule_fingerprint: spec.market_rule_fingerprint.clone(),
                reason_code: None,
                reason: None,
            },
            spec,
            catalog_fingerprint: "c".repeat(64),
        }
    }

    #[test]
    fn generated_catalog_allows_only_a_code_tested_binance_sandbox_scope() {
        let catalog = qx_api::product_capability_catalog_document().expect("generated catalog");
        let spec = ExecutionCapabilityPreflightSpecV1 {
            schema_version: EXECUTION_CAPABILITY_PREFLIGHT_SCHEMA_VERSION,
            profile: ExecutionProfile::SandboxAcceptance,
            capability_id: "qx.venue.binance_spot".into(),
            venue: "BINANCE".into(),
            product: "spot".into(),
            account_mode: "cash".into(),
            account_model: "spot".into(),
            margin_mode: "cash".into(),
            position_mode: "not_applicable".into(),
            order_type: "market".into(),
            quantity_unit: "base".into(),
            side: "buy".into(),
            market_rule_fingerprint: "b".repeat(64),
            sandbox_endpoint: true,
        };
        let context = RunContext::new(CallerCapability::Live, "catalog-contract");
        let decision = evaluate_execution_capability_preflight_v1(&spec, &catalog, &context)
            .expect("well-formed request");

        assert!(decision.allowed, "{decision:?}");
        assert!(decision
            .maturity_refs
            .iter()
            .any(|value| value == "binance_direct"));
        assert_eq!(
            decision.evidence_stages["binance_direct"]["sandbox_tested"],
            false
        );
    }

    fn reconciler_worker(venue_id: &str) -> WorkerConfig {
        serde_json::from_value(json!({
            "id": "query-reconciler",
            "role": "reconciler",
            "enabled": true,
            "account_id": "main",
            "venue_id": venue_id,
            "symbols": []
        }))
        .expect("valid reconciler worker")
    }

    #[test]
    fn binance_reconciler_private_queries_require_testnet_scope() {
        let worker = reconciler_worker("binance-testnet");
        let decision = require_account_query_execution_capability(
            &worker,
            "sandbox",
            AccountQueryPurpose::Reconcile,
            ExecutionConnectorProfile::BinanceSpot,
        )
        .expect("declared testnet account-read scope");
        assert!(decision.allowed);
        assert_eq!(decision.allowed_operations, ["balance.read", "orders.read"]);

        let mainnet_worker = reconciler_worker("binance");
        let error = require_account_query_execution_capability(
            &mainnet_worker,
            "sandbox",
            AccountQueryPurpose::Reconcile,
            ExecutionConnectorProfile::BinanceSpot,
        )
        .expect_err("sandbox profile cannot reach a mainnet endpoint");
        assert!(error.contains("sandbox_endpoint_required"), "{error}");
    }

    struct TempCcxtQueryConfig(std::path::PathBuf);

    impl Drop for TempCcxtQueryConfig {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    impl TempCcxtQueryConfig {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    fn write_ccxt_query_config(value: Value) -> TempCcxtQueryConfig {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::current_dir().unwrap().join(format!(
            "qianxing-account-query-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        TempCcxtQueryConfig(path)
    }

    #[test]
    fn ccxt_private_query_scope_uses_exchange_account_model_and_product() {
        let worker = reconciler_worker("bitget");
        let config = write_ccxt_query_config(json!({
            "exchange_id": "bitget",
            "account_model": "uta",
            "default_type": "spot",
            "sandbox": true,
            "credential_env": {"api_key": "QX_TEST_KEY", "secret": "QX_TEST_SECRET"},
            "options": {"uta": true}
        }));
        let decision = require_account_query_execution_capability(
            &worker,
            "sandbox",
            AccountQueryPurpose::Reconcile,
            ExecutionConnectorProfile::Ccxt {
                config_path: config.path(),
            },
        )
        .expect("declared Bitget UTA spot scope");
        assert!(decision.allowed);
        assert_eq!(decision.product, "spot");
        assert_eq!(decision.account_model, "uta");
        assert_eq!(
            decision.allowed_operations,
            ["balance.read", "cashflows.read", "orders.read"]
        );
    }

    #[test]
    fn ccxt_derivative_private_queries_require_positions_and_expose_optional_funding_reads() {
        let worker = reconciler_worker("okx");
        let config = write_ccxt_query_config(json!({
            "exchange_id": "okx",
            "account_model": "unified",
            "default_type": "swap",
            "sandbox": true,
            "credential_env": {"api_key": "QX_TEST_KEY", "secret": "QX_TEST_SECRET", "password": "QX_TEST_PASS"},
            "options": {}
        }));
        let decision = require_account_query_execution_capability(
            &worker,
            "sandbox",
            AccountQueryPurpose::Reconcile,
            ExecutionConnectorProfile::Ccxt {
                config_path: config.path(),
            },
        )
        .expect("declared OKX unified perpetual scope");
        assert!(decision.allowed);
        assert_eq!(decision.product, "perpetual");
        assert!(decision
            .allowed_operations
            .contains(&"positions.read".into()));
        assert!(decision.allowed_operations.contains(&"funding.read".into()));
    }

    #[test]
    fn bitget_account_model_must_match_explicit_uta_switch() {
        let uta = json!({
            "account_model": "uta",
            "default_type": "swap",
            "options": {"uta": true}
        });
        let classic = json!({
            "account_model": "classic",
            "default_type": "spot",
            "options": {"uta": false}
        });

        assert_eq!(
            declared_ccxt_account_model(&uta, "BITGET", TradingProduct::Perpetual).unwrap(),
            "uta"
        );
        assert_eq!(
            declared_ccxt_account_model(&classic, "BITGET", TradingProduct::Spot).unwrap(),
            "classic"
        );
        let mismatched = json!({
            "account_model": "classic",
            "default_type": "swap",
            "options": {"uta": true}
        });
        assert!(
            declared_ccxt_account_model(&mismatched, "BITGET", TradingProduct::Perpetual).is_err()
        );
    }

    #[test]
    fn ccxt_execution_requires_explicit_model_and_product_configuration() {
        let implicit = json!({"default_type": "spot"});
        let wrong_product = json!({
            "account_model": "usd_m_futures",
            "default_type": "spot"
        });

        assert!(declared_ccxt_account_model(&implicit, "BINANCE", TradingProduct::Spot).is_err());
        assert!(
            declared_ccxt_account_model(&wrong_product, "BINANCE", TradingProduct::Perpetual)
                .is_err()
        );
    }

    #[test]
    fn order_fingerprint_binds_requested_terms_but_ignores_live_progress() {
        let order = test_order();
        let original = order_fingerprint(&order).unwrap();
        let mut progressed = order.clone();
        progressed.status = OrderStatus::PartiallyFilled;
        progressed.filled = Quantity::from_i64(1);
        assert_eq!(order_fingerprint(&progressed).unwrap(), original);

        let mut changed = order;
        changed.qty = Quantity::from_i64(3);
        assert_ne!(order_fingerprint(&changed).unwrap(), original);
    }

    #[test]
    fn persisted_decision_keeps_the_complete_snapshot_and_command_binding() {
        let order = test_order();
        let resolved = test_resolved();
        let fact = execution_decision_fact(
            &order,
            &resolved,
            ExecutionCapabilityDecisionKind::Submit,
            Some(73),
            Some("spread-41".into()),
        )
        .unwrap();
        assert_eq!(fact.command_id, Some(73));
        assert_eq!(fact.order_group_id.as_deref(), Some("spread-41"));
        assert_eq!(fact.client_order_id, order.client_id);
        assert_eq!(fact.account_id, order.account_id);
        assert_eq!(fact.venue_id, "BINANCE");
        assert_eq!(fact.catalog_fingerprint, "c".repeat(64));
        assert!(fact.preflight_json.contains("account_model"));
        assert!(fact.preflight_json.contains("code_tested"));
        assert!(same_preflight_binding(
            &fact,
            &execution_decision_fact(
                &order,
                &resolved,
                ExecutionCapabilityDecisionKind::RecoveryAdoption,
                None,
                None,
            )
            .unwrap()
        ));

        let mut changed = resolved;
        changed.spec.market_rule_fingerprint = "d".repeat(64);
        changed.decision.market_rule_fingerprint = "d".repeat(64);
        assert!(!same_preflight_binding(
            &fact,
            &execution_decision_fact(
                &order,
                &changed,
                ExecutionCapabilityDecisionKind::RecoveryAdoption,
                None,
                None,
            )
            .unwrap()
        ));
    }
}
