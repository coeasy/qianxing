use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy)]
enum FailurePoint {
    Init,
    Start,
    Event,
    Stop,
}

#[derive(Default)]
struct Calls {
    starts: AtomicUsize,
    events: AtomicUsize,
    stops: AtomicUsize,
}

struct FailingLifecycle {
    point: FailurePoint,
    fail_stop: bool,
    calls: Arc<Calls>,
}

impl Strategy for FailingLifecycle {
    fn on_init(&mut self, _: &StrategyContext) -> Result<(), String> {
        if matches!(self.point, FailurePoint::Init) {
            Err("injected on_init failure".into())
        } else {
            Ok(())
        }
    }

    fn on_start(&mut self, _: &StrategyContext) -> Result<(), String> {
        self.calls.starts.fetch_add(1, Ordering::SeqCst);
        if matches!(self.point, FailurePoint::Start) {
            Err("injected on_start failure".into())
        } else {
            Ok(())
        }
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        self.calls.events.fetch_add(1, Ordering::SeqCst);
        if matches!(self.point, FailurePoint::Event) {
            return Err("injected on_event failure".into());
        }
        Ok(StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("{}:{}", context.strategy_id, event.ts()),
            strategy_id: context.strategy_id.clone(),
            signal_id: event.ts(),
            confidence: 10_000,
            priority: 0,
            expires_at: 0,
            intents: Vec::new(),
        })
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.calls.stops.fetch_add(1, Ordering::SeqCst);
        if self.fail_stop {
            Err("injected on_stop failure".into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn strategy_lifecycle_failures_stop_cleanly_without_publishing_partial_artifacts() {
    let root = scratch("multi-leg-lifecycle-failures");
    let primary = write_frame(&root, "primary.json", "BTCUSDT.BINANCE", 8);
    let reference = write_frame(&root, "reference.json", "ETHUSDT.KRAKEN", 8);

    for (name, point, expected_start_calls, expected_event_calls) in [
        ("init", FailurePoint::Init, 0, Some(0)),
        ("start", FailurePoint::Start, 1, Some(0)),
        ("event", FailurePoint::Event, 1, Some(1)),
        ("stop", FailurePoint::Stop, 1, None),
    ] {
        let output = root.join(name);
        let spec = MultiLegBacktestSpec::new(
            qx_app::MultiLegDatasetInput {
                run_id: format!("multi-leg-failure-{name}"),
                primary_bars_path: primary.clone(),
                reference_bars_path: reference.clone(),
            },
            "USDT",
            1_000 * SCALE,
            1_000 * SCALE,
            7,
            output.to_string_lossy(),
            BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "lifecycle-failure"),
        );
        let calls = Arc::new(Calls::default());
        let identity = qx_app::StrategyArtifactIdentity {
            strategy_id: "lifecycle-failure".into(),
            strategy_version: "failure-matrix-v1".into(),
            source_sha256: "a".repeat(64),
        };
        let error = qx_app::run_multi_leg_backtest_with_strategy(
            &spec,
            &identity,
            Box::new(FailingLifecycle {
                point,
                fail_stop: matches!(point, FailurePoint::Stop),
                calls: calls.clone(),
            }),
            &context(&format!("multi-leg-failure-{name}")),
        )
        .expect_err("injected strategy lifecycle failure must fail the run");

        assert!(
            error.to_string().contains("injected on_"),
            "the original callback failure must remain visible: {error}"
        );
        assert_eq!(
            calls.starts.load(Ordering::SeqCst),
            expected_start_calls,
            "case={name}"
        );
        let event_calls = calls.events.load(Ordering::SeqCst);
        if let Some(expected) = expected_event_calls {
            assert_eq!(event_calls, expected, "case={name}");
        } else {
            assert!(event_calls > 0, "case={name} must reach on_event");
        }
        assert_eq!(calls.stops.load(Ordering::SeqCst), 1, "case={name}");
        assert!(
            !output.exists(),
            "case={name} must not publish partial multi-leg artifacts: {}",
            output.display()
        );
    }

    let output = root.join("init-and-stop");
    let spec = MultiLegBacktestSpec::new(
        qx_app::MultiLegDatasetInput {
            run_id: "multi-leg-failure-init-and-stop".into(),
            primary_bars_path: primary,
            reference_bars_path: reference,
        },
        "USDT",
        1_000 * SCALE,
        1_000 * SCALE,
        7,
        output.to_string_lossy(),
        BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "lifecycle-failure"),
    );
    let calls = Arc::new(Calls::default());
    let error = qx_app::run_multi_leg_backtest_with_strategy(
        &spec,
        &qx_app::StrategyArtifactIdentity {
            strategy_id: "lifecycle-failure".into(),
            strategy_version: "failure-matrix-v1".into(),
            source_sha256: "a".repeat(64),
        },
        Box::new(FailingLifecycle {
            point: FailurePoint::Init,
            fail_stop: true,
            calls: calls.clone(),
        }),
        &context("multi-leg-failure-init-and-stop"),
    )
    .expect_err("init and cleanup failures must both be reported");
    assert!(error.to_string().contains("injected on_init failure"));
    assert!(error
        .to_string()
        .contains("on_stop 也失败: injected on_stop failure"));
    assert_eq!(calls.stops.load(Ordering::SeqCst), 1);
    assert!(!output.exists());
}

struct BlockingLifecycle {
    entered: Sender<()>,
    release: Receiver<()>,
    calls: Arc<Calls>,
    blocked: bool,
}

impl Strategy for BlockingLifecycle {
    fn on_start(&mut self, _: &StrategyContext) -> Result<(), String> {
        self.calls.starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        self.calls.events.fetch_add(1, Ordering::SeqCst);
        if !self.blocked {
            self.blocked = true;
            self.entered
                .send(())
                .map_err(|_| "test cancellation receiver dropped")?;
            self.release
                .recv_timeout(Duration::from_secs(5))
                .map_err(|error| format!("test did not release callback: {error}"))?;
        }
        Ok(StrategyDecision {
            schema_version: STRATEGY_API_VERSION,
            request_id: format!("{}:{}", context.strategy_id, event.ts()),
            strategy_id: context.strategy_id.clone(),
            signal_id: event.ts(),
            confidence: 10_000,
            priority: 0,
            expires_at: 0,
            intents: Vec::new(),
        })
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.calls.stops.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn cancellation_waits_for_callback_then_stops_without_publishing_artifacts() {
    let root = scratch("multi-leg-cancellation");
    let primary = write_frame(&root, "primary.json", "BTCUSDT.BINANCE", 80);
    let reference = write_frame(&root, "reference.json", "ETHUSDT.KRAKEN", 80);
    let output = root.join("out");
    let spec = MultiLegBacktestSpec::new(
        qx_app::MultiLegDatasetInput {
            run_id: "multi-leg-cancellation".into(),
            primary_bars_path: primary,
            reference_bars_path: reference,
        },
        "USDT",
        1_000 * SCALE,
        1_000 * SCALE,
        7,
        output.to_string_lossy(),
        BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "cancellation"),
    );
    let identity = qx_app::StrategyArtifactIdentity {
        strategy_id: "cancellation".into(),
        strategy_version: "cancellation-v1".into(),
        source_sha256: "b".repeat(64),
    };
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let calls = Arc::new(Calls::default());
    let mut handle = spec
        .start_with_strategy(
            identity,
            BlockingLifecycle {
                entered: entered_tx,
                release: release_rx,
                calls: calls.clone(),
                blocked: false,
            },
            context("multi-leg-cancellation"),
        )
        .expect("cancellable strategy run starts");

    entered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("strategy callback enters before cancellation");
    handle.cancel();
    release_tx.send(()).expect("release blocked callback");
    assert_eq!(handle.wait(), qx_app::RunStatus::Cancelled);
    assert_eq!(calls.starts.load(Ordering::SeqCst), 1);
    assert!(calls.events.load(Ordering::SeqCst) >= 1);
    assert_eq!(calls.stops.load(Ordering::SeqCst), 1);
    assert!(!output.exists(), "cancelled run must publish no artifacts");
    let events = handle.events_after(0);
    assert!(events.iter().any(|event| {
        event.kind == qx_app::RunEventKind::CancellationRequested
            && event.status == qx_app::RunStatus::Cancelling
    }));
    assert!(events.iter().any(|event| {
        event.kind == qx_app::RunEventKind::Cancelled
            && event.status == qx_app::RunStatus::Cancelled
    }));
}
