//! 内置生态 smoke：合成行情、演示数据源与纸面交易的一次性自检链路。
//!
//! 这里的回测只提供**合成输入**，撮合与账本走 `qx-xingban` 真实内核（与 `backtest
//! builtin` 同一装配），CLI 内不再自带第二套撮合引擎。
//!
//! 由 `main.rs` 的 crate 根职责簇拆出（Phase 4p），条目经根部的
//! `pub(crate) use ecosystem_smoke::*;` 再导出。

use crate::default_execution_cost_binding;
use qx_core::{
    InstrumentId, Money, Order, OrderStatus, Price, Quantity, Side, DEFAULT_SETTLEMENT_CURRENCY,
};
use qx_guanxing::{Bar, DataSourceId, QuoteTick};
use qx_provider::{
    DataProvider, DataQuery, ProviderCapability, ProviderError, ProviderErrorClass, ProviderResult,
};
use qx_strategy::{BuiltinStrategyConfig, BuiltinStrategyKind};
use qx_xingban::DeterministicRng;
use qx_zhenlu::{PaperVenue, Venue, VenueEvent};

mod chain;
mod provider_capability;
pub(crate) use provider_capability::provider_capability;
mod json_output;
pub(crate) use json_output::{print_json_report, print_machine_json};
mod formatting;
pub(crate) use formatting::f;

pub(crate) struct DemoProvider {
    capability: ProviderCapability,
    fail: bool,
}

impl DataProvider for DemoProvider {
    fn capability(&self) -> &ProviderCapability {
        &self.capability
    }

    fn fetch(&self, _query: &DataQuery) -> Result<ProviderResult, ProviderError> {
        if self.fail {
            return Err(ProviderError::new(
                ProviderErrorClass::SwitchProvider,
                "demo primary unavailable",
            ));
        }
        let payload = qx_provider::ProviderPayload::new_json(
            "BTCUSDT.BINANCE",
            "application/vnd.qianxing.bar+json;v=1",
            r#"{"ts":1,"open":99,"high":102,"low":98,"close":100,"volume":1000}"#,
        );
        let mut result = ProviderResult {
            schema_version: qx_provider::PROVIDER_RESULT_SCHEMA_VERSION,
            records: vec![qx_guanxing::RawRecord {
                source: DataSourceId::new(self.capability.provider_id.clone()),
                event_time: 1,
                receive_time: 2,
                payload_hash: payload.digest(),
                schema_version: 1,
            }],
            payloads: vec![payload],
            provider_id: self.capability.provider_id.clone(),
            provider_version: self.capability.version.clone(),
            request_id: "cli-ecosystem".into(),
            retry_chain: Vec::new(),
            received_at: 2,
            source_hash: 0,
        };
        result.source_hash = result.compute_source_hash();
        Ok(result)
    }
}

/// 生成确定性合成行情（同一 seed 必然产生同一序列）。
pub(crate) fn gen_bars(n: usize, seed: u64) -> Vec<Bar> {
    let mut rng = DeterministicRng::new(seed);
    let mut bars = Vec::with_capacity(n);
    let mut px: i128 = 100_000_000_000; // 100.0

    for i in 0..n {
        let drift = ((rng.next_u64() % 2001) as i128) - 1000;
        px = (px + drift * 1_000_000).max(1_000_000_000);
        let open = px;
        let close = px + (((rng.next_u64() % 1001) as i128) - 500) * 1_000_000;
        let high = open.max(close) + ((rng.next_u64() % 501) as i128) * 1_000_000;
        let low = (open.min(close) - ((rng.next_u64() % 501) as i128) * 1_000_000).max(1_000_000);
        let volume = 1_000 + (rng.next_u64() % 5_000) as i128;
        bars.push(Bar::new(
            (i as u64 + 1) * 1_000_000_000,
            open,
            high,
            low,
            close,
            volume,
        ));
    }
    bars
}

pub(crate) struct Outcome {
    pub(crate) hash: u64,
    pub(crate) n_fills: usize,
    pub(crate) total_fee: i128,
    pub(crate) return_bps: i32,
    pub(crate) max_drawdown_bps: u32,
    pub(crate) final_equity: i128,
}

pub(crate) fn run_ecosystem_smoke(as_json: bool) -> Result<(), String> {
    if as_json {
        let outcome = chain::run_ecosystem_smoke_inner(true);
        let failure = outcome.as_ref().err().cloned();
        let succeeded = failure.is_none();
        let checks = vec![serde_json::json!({
            "name": "ecosystem_smoke_chain",
            "status": if succeeded { "pass" } else { "blocked" }
        })];
        let report = serde_json::json!({
            "schema_version": 2,
            "command": "ecosystem",
            "ok": succeeded,
            "readiness_layers": {
                "R": {"status": if succeeded { "pass" } else { "blocked" }, "scope": "synthetic_ecosystem_smoke", "evidence": if succeeded { vec!["factor_catalog", "qifi_snapshot_roundtrip", "provider_failover", "scheduler_readiness", "control_plane", "query_api_routes", "foundation_specs"] } else { Vec::<&str>::new() }, "detail": "生态组件合成数据冒烟；不代表真实行情或生产部署就绪"},
                "P": {"status": "not_assessed", "scope": "paper_execution", "evidence": Vec::<&str>::new(), "detail": "ecosystem 不运行 PaperVenue 订单生命周期；使用 all 或 paper-check 执行对应检查"},
                "O": {"status": "not_assessed", "scope": "operator_runtime", "evidence": Vec::<&str>::new(), "detail": "未启动或检查运行时 worker"},
                "L": {"status": "not_assessed", "scope": "live_account", "evidence": Vec::<&str>::new(), "detail": "未连接真实账户或提交真实订单"}
            },
            "checks": checks,
            "warnings": [],
            "failures": failure.into_iter().collect::<Vec<_>>(),
            "network_accessed": false,
            "orders_sent": false
        });
        print_json_report(&report)?;
        if !succeeded {
            std::process::exit(1);
        }
        return Ok(());
    }
    chain::run_ecosystem_smoke_inner(false)
}

pub(crate) fn mk_order(id: u64, instr: &InstrumentId, side: Side, qty: i64) -> Order {
    Order {
        client_id: id,
        instrument: instr.clone(),
        side,
        qty: Quantity::from_i64(qty),
        limit: None,
        status: OrderStatus::Submitted,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
        exact_amounts: None,
    }
}

/// 合成行情 + **真实撮合内核**的双均线回测产物。
///
/// 引擎与 `backtest builtin` 同源：执行委托 qx-app 内置策略用例，配置 next-open 成交与
/// maker/taker 2/5bp 费率，策略使用内置 `sma_cross`。本文件只生成合成输入。
pub(crate) fn run_backtest(
    bars: &[Bar],
    seed: u64,
    fast: usize,
    slow: usize,
) -> Result<Outcome, String> {
    let instrument = InstrumentId::parse("DEMO.SIM")
        .ok_or_else(|| "自检回测示例标的无效: DEMO.SIM".to_string())?;
    let strategy_config = BuiltinStrategyConfig {
        fast_window: fast,
        slow_window: slow,
        ..BuiltinStrategyConfig::new(
            BuiltinStrategyKind::SmaCross,
            "cli-selfcheck-sma",
            instrument.clone(),
            Quantity::from_i64(10),
        )
        .map_err(|error| format!("内置双均线参数无效: {error}"))?
    };
    let initial_cash = Money::from_i64(crate::backtests::DEFAULT_BACKTEST_INITIAL_CASH);
    let costs = default_execution_cost_binding();
    let strategy_spec = qx_app::BuiltinStrategySpec::from_strategy_config(&strategy_config)
        .map_err(|error| format!("自检内置策略规格无效: {error}"))?;
    let spec = qx_app::assemble_strategy_backtest_spec(
        qx_app::BacktestDatasetInput {
            run_id: "ecosystem-smoke".into(),
            instrument: instrument.to_string(),
            bars_path: "synthetic:ecosystem-smoke".into(),
        },
        strategy_spec,
        qx_app::StrategyBacktestBindings {
            settlement_currency: DEFAULT_SETTLEMENT_CURRENCY.into(),
            initial_cash_raw: initial_cash.raw(),
            seed,
            output_dir: ".".into(),
            account_id: "main".into(),
            market_spec: None,
            margin_model: None,
            execution_cost_rules: Some(costs.rules.clone()),
            ashare_rules: None,
            risk_rules: None,
            allow_short: false,
            fill_model: qx_app::BacktestFillModel::NextBarOpen,
            max_bar_volume_participation_bps: None,
        },
    );
    let context = qx_app::RunContext::new(qx_app::CallerCapability::Research, "ecosystem-smoke")
        .with_code_commit(crate::build_identity::BUILD_REVISION);
    let outcome = qx_app::run_native_builtin_strategy_backtest_with_outcome(
        &spec,
        bars,
        strategy_config,
        qx_app::NativeStrategyBacktestContextBindings {
            account_id: "main".into(),
            venue_id: instrument.venue.to_string(),
            data_fingerprint: format!("synthetic:{}", bars.len()),
            as_of: bars.first().map(|bar| bar.ts).unwrap_or(1),
            settlement_currency: DEFAULT_SETTLEMENT_CURRENCY.into(),
            initial_cash_raw: initial_cash.raw(),
        },
        &context,
    )
    .map_err(|error| format!("合成输入的真实内核回测失败: {error}"))?;
    let report = outcome.report;
    Ok(Outcome {
        hash: report.result_hash(),
        n_fills: report.fills.len(),
        total_fee: report.fees_raw,
        return_bps: report.return_bps,
        max_drawdown_bps: report.max_drawdown_bps,
        final_equity: report.final_equity(),
    })
}

pub(crate) fn run_paper_smoke(print_output: bool) -> Result<(), String> {
    let instrument = InstrumentId::parse("DEMO.SIM")
        .ok_or_else(|| "Paper 冒烟示例标的无效: DEMO.SIM".to_string())?;
    let mut venue = PaperVenue::new("paper", default_execution_cost_binding().fee_model());
    let order = mk_order(9001, &instrument, Side::Buy, 2);
    let accepted = venue
        .submit(order, 1)
        .map_err(|error| format!("Paper 订单提交失败: {error}"))?;
    if !matches!(accepted.as_slice(), [VenueEvent::Accepted { .. }]) {
        return Err(format!("Paper 订单接受事件异常: {accepted:?}"));
    }
    let events = venue
        .try_on_quote(
            &instrument,
            QuoteTick::new(
                2,
                Price::from_i64(99),
                Quantity::from_i64(10),
                Price::from_i64(100),
                Quantity::from_i64(10),
                1,
            ),
        )
        .map_err(|error| format!("Paper 行情撮合失败: {error}"))?;
    if !matches!(events.as_slice(), [VenueEvent::Fill(_)]) {
        return Err(format!("Paper 报价成交事件异常: {events:?}"));
    }
    if venue.snapshot().first().map(|order| order.status) != Some(OrderStatus::Filled) {
        return Err("Paper 成交后订单状态不是 Filled".into());
    }

    let remote_snapshot = venue.snapshot();
    venue.disconnect();
    if venue
        .submit(mk_order(9002, &instrument, Side::Buy, 1), 3)
        .is_ok()
    {
        return Err("Paper 断线后仍接受新订单".into());
    }
    if venue.state() != qx_zhenlu::ConnectorState::ReconcileRequired {
        return Err(format!("Paper 断线后状态异常: {:?}", venue.state()));
    }
    venue.reconnect();
    if !venue.connected() {
        return Err("Paper reconnect 后仍未连接".into());
    }
    let reconciliation = venue
        .reconcile_snapshot(&remote_snapshot)
        .map_err(|error| format!("Paper 快照对账失败: {error}"))?;
    if !reconciliation.is_empty() {
        return Err(format!("Paper 快照对账产生差异: {reconciliation:?}"));
    }
    if print_output {
        println!("[针路 · PaperVenue] 订单接受/报价成交/断线转对账/恢复通过 ✓");
    }
    Ok(())
}
