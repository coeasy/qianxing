use super::*;
use qx_strategy::{StrategyErrorStage, StrategyErrorV1};

struct ErrorObservingStrategy {
    errors: Arc<Mutex<Vec<StrategyErrorV1>>>,
    fail_event: bool,
    fail_fill_update: bool,
    emitted: bool,
}

impl Strategy for ErrorObservingStrategy {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &qx_strategy::MarketEvent,
    ) -> Result<StrategyDecision, String> {
        if self.fail_event {
            return Err("original order-book market failure".into());
        }
        if !self.emitted {
            self.emitted = true;
            return NativeBuy.on_event(context, event);
        }
        Ok(StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("error-observer:{}", event.ts()),
            strategy_id: context.strategy_id.clone(),
            signal_id: event.ts(),
            confidence: 1,
            priority: 0,
            expires_at: event.ts(),
            intents: Vec::new(),
        })
    }

    fn on_order_update(
        &mut self,
        _context: &StrategyContext,
        _update: &str,
    ) -> Result<Option<StrategyDecision>, String> {
        if self.fail_fill_update {
            Err("original order-book fill update failure".into())
        } else {
            Ok(None)
        }
    }

    fn on_error(&mut self, error: &StrategyErrorV1) -> Result<(), String> {
        self.errors.lock().unwrap().push(error.clone());
        Err("diagnostic observer failure must be ignored".into())
    }
}

fn assert_orderbook_error(
    strategy: &mut dyn OrderBookStrategy,
    errors: &Arc<Mutex<Vec<StrategyErrorV1>>>,
    stage: StrategyErrorStage,
    expected_message: &str,
) {
    let result = OrderBookBacktestEngine::new(l2_config(0))
        .run(&[snapshot(1, 100), snapshot(2, 101)], strategy);
    let error = match result {
        Err(error) => error.to_string(),
        Ok(_) => panic!("strategy callback failure should abort the backtest"),
    };
    assert!(error.contains(expected_message), "{error}");
    let observed = errors.lock().unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].stage, stage);
    assert_eq!(
        observed[0].timestamp,
        Some(if stage == StrategyErrorStage::MarketEvent {
            1
        } else {
            2
        })
    );
    assert_eq!(observed[0].instrument.as_deref(), Some("BTCUSDT.BINANCE"));
    assert_eq!(observed[0].message, expected_message);
}

#[test]
fn native_orderbook_and_depth_bar_adapters_report_market_callback_failures() {
    let native_errors = Arc::new(Mutex::new(Vec::new()));
    let mut native = NativeOrderBookStrategy::new(
        ErrorObservingStrategy {
            errors: Arc::clone(&native_errors),
            fail_event: true,
            fail_fill_update: false,
            emitted: false,
        },
        book_context(),
    );
    assert_orderbook_error(
        &mut native,
        &native_errors,
        StrategyErrorStage::MarketEvent,
        "original order-book market failure",
    );

    let depth_errors = Arc::new(Mutex::new(Vec::new()));
    let mut depth = DepthBarStrategy::new(
        ErrorObservingStrategy {
            errors: Arc::clone(&depth_errors),
            fail_event: true,
            fail_fill_update: false,
            emitted: false,
        },
        book_context(),
        snapshot(1, 100).instrument,
    );
    assert_orderbook_error(
        &mut depth,
        &depth_errors,
        StrategyErrorStage::MarketEvent,
        "original order-book market failure",
    );
}

#[test]
fn native_orderbook_adapter_reports_fill_update_callback_failure() {
    let errors = Arc::new(Mutex::new(Vec::new()));
    let mut native = NativeOrderBookStrategy::new(
        ErrorObservingStrategy {
            errors: Arc::clone(&errors),
            fail_event: false,
            fail_fill_update: true,
            emitted: false,
        },
        book_context(),
    );
    assert_orderbook_error(
        &mut native,
        &errors,
        StrategyErrorStage::FillUpdate,
        "original order-book fill update failure",
    );
}

#[test]
fn depth_bar_view_lets_bar_only_strategies_trade_on_order_book_snapshots() {
    let mut strategy =
        DepthBarStrategy::new(NativeBuy, book_context(), snapshot(1, 100).instrument);
    let report = OrderBookBacktestEngine::new(l2_config(0))
        .run(&[snapshot(1, 100), snapshot(2, 101)], &mut strategy)
        .unwrap();
    assert_eq!(report.fills.len(), 1);
    assert_eq!(report.pending_orders, 1);
    assert_eq!(report.positions, vec![0, Quantity::from_i64(1).raw()]);
}

struct WarmupNativeBuy {
    ready: bool,
}

impl Strategy for WarmupNativeBuy {
    fn history_requirements(&self) -> Result<StrategyHistoryRequirements, String> {
        Ok(StrategyHistoryRequirements { warmup_bars: 2 })
    }

    fn on_warmup_complete(
        &mut self,
        _context: &StrategyContext,
        _instrument: Option<&InstrumentId>,
        observed_bars: u64,
    ) -> Result<(), String> {
        assert_eq!(observed_bars, 2);
        self.ready = true;
        Ok(())
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &qx_strategy::MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let mut decision = NativeBuy.on_event(context, event)?;
        if !self.ready {
            decision.intents.clear();
        }
        Ok(decision)
    }
}

#[test]
fn depth_bar_view_suppresses_warmup_orders_until_ready() {
    let mut strategy = DepthBarStrategy::new(
        WarmupNativeBuy { ready: false },
        book_context(),
        snapshot(1, 100).instrument,
    );
    let report = OrderBookBacktestEngine::new(l2_config(0))
        .run(
            &[snapshot(1, 100), snapshot(2, 101), snapshot(3, 102)],
            &mut strategy,
        )
        .unwrap();
    assert_eq!(report.fills.len(), 1);
    assert_eq!(report.fills[0].ts, 3);
}

#[test]
fn raw_order_book_host_rejects_bar_warmup_declaration() {
    let mut strategy =
        NativeOrderBookStrategy::new(WarmupNativeBuy { ready: false }, book_context());
    let error = OrderBookStrategy::on_start(&mut strategy).unwrap_err();
    assert!(error.contains("warmup_bars"));
}

struct StopReasonObserver {
    received: Arc<Mutex<Option<qx_strategy::StrategyStopContextV1>>>,
    fail_on_event: bool,
}

impl Strategy for StopReasonObserver {
    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &qx_strategy::MarketEvent,
    ) -> Result<StrategyDecision, String> {
        if self.fail_on_event {
            return Err("intentional event failure".into());
        }
        Ok(StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("stop-observer:{}", event.ts()),
            strategy_id: context.strategy_id.clone(),
            signal_id: event.ts(),
            confidence: 10_000,
            priority: 0,
            expires_at: event.ts(),
            intents: Vec::new(),
        })
    }

    fn on_stop_with_context(
        &mut self,
        stop: &qx_strategy::StrategyStopContextV1,
    ) -> Result<(), String> {
        *self.received.lock().unwrap() = Some(stop.clone());
        Ok(())
    }
}

fn run_stop_reason_case(
    reason: qx_strategy::StrategyStopReason,
    cancelled: bool,
    fail_on_event: bool,
) {
    let received = Arc::new(Mutex::new(None));
    let mut strategy = DepthBarStrategy::new(
        StopReasonObserver {
            received: Arc::clone(&received),
            fail_on_event,
        },
        book_context(),
        snapshot(1, 100).instrument,
    );
    let result = OrderBookBacktestEngine::new(l2_config(0)).run_with_cancel(
        &[snapshot(1, 100), snapshot(2, 101)],
        &mut strategy,
        || cancelled,
    );
    if fail_on_event {
        assert!(result.is_err());
    } else if cancelled {
        assert!(result.unwrap().is_none());
    } else {
        assert!(result.unwrap().is_some());
    }
    let stop = received.lock().unwrap().clone().unwrap();
    assert_eq!(stop.reason, reason);
    assert_eq!(
        stop.context
            .as_ref()
            .map(|context| context.strategy_id.as_str()),
        Some("depth-book")
    );
}

#[test]
fn depth_bar_host_passes_completed_cancelled_and_failed_stop_reasons() {
    run_stop_reason_case(qx_strategy::StrategyStopReason::Completed, false, false);
    run_stop_reason_case(qx_strategy::StrategyStopReason::Cancelled, true, false);
    run_stop_reason_case(qx_strategy::StrategyStopReason::Failed, false, true);
}
