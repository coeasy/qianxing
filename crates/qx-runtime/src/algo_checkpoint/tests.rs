use super::*;
use qx_core::{InstrumentId, Order, OrderStatus, Price, Quantity, Side};
use qx_execution::{
    AlgoExecutionStep, AlgoParentOrder, EventAppender, ExecutionEvent, ExecutionEventEnvelope,
    ExecutionGateway, OrderStore, RiskVerdict, VenuePort,
};
use std::sync::{Arc, Barrier};

struct TestVenue;

impl VenuePort for TestVenue {
    fn venue_id(&self) -> &str {
        "paper-test"
    }

    fn submit_order(&mut self, order: Order, _ts: u64) -> Result<Vec<ExecutionEvent>, String> {
        Ok(vec![ExecutionEvent::Accepted {
            client_order_id: order.client_id,
            venue_order_id: format!("paper-{}", order.client_id),
        }])
    }

    fn cancel_order(
        &mut self,
        client_order_id: u64,
        _ts: u64,
    ) -> Result<Vec<ExecutionEvent>, String> {
        Ok(vec![ExecutionEvent::Cancelled { client_order_id }])
    }
}

#[derive(Default)]
struct TestEvents(Vec<Order>);

impl OrderStore for TestEvents {
    fn orders(&self) -> Vec<Order> {
        self.0.clone()
    }

    fn register_order(
        &mut self,
        order: Order,
        _ts: u64,
        _correlation_id: Option<String>,
    ) -> Result<(), String> {
        self.0.push(order);
        Ok(())
    }
}

impl EventAppender for TestEvents {
    fn append_execution_event(&mut self, envelope: ExecutionEventEnvelope) -> Result<(), String> {
        if let ExecutionEvent::Accepted {
            client_order_id, ..
        } = envelope.event
        {
            let order = self
                .0
                .iter_mut()
                .find(|order| order.client_id == client_order_id)
                .ok_or_else(|| "Accepted event without a registered order".to_string())?;
            order.status = OrderStatus::Accepted;
        }
        Ok(())
    }
}

struct AllowRisk;

impl RiskPort for AllowRisk {
    fn evaluate_order(&self, _order: &Order) -> Result<RiskVerdict, String> {
        Ok(RiskVerdict::Allow)
    }
}

fn root(label: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "qx-algo-checkpoint-{label}-{}-{unique}",
        std::process::id()
    ))
}

fn initial_state() -> AlgoExecutionState {
    let parent_id = "parent-恢复/with unsafe path chars";
    let plan = qx_execution::plan_twap(parent_id, 11, 10, 50, 4).unwrap();
    AlgoExecutionState::new(AlgoParentOrder {
        parent_id: parent_id.into(),
        plan,
        child_order_id_base: 100,
        parent_order: Order {
            client_id: 90,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_raw(11),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        },
    })
    .unwrap()
}

fn advance_once(store: AlgoExecutionCheckpointStore) -> AlgoExecutionStep {
    let mut venue = TestVenue;
    let mut events = TestEvents::default();
    let mut source_seq = 0;
    let risk = AllowRisk;
    let mut gateway = ExecutionGateway::new(
        &mut venue,
        &mut events,
        "checkpoint-test",
        10,
        &mut source_seq,
    );
    store
        .step(
            "parent-恢复/with unsafe path chars",
            10,
            &mut gateway,
            &risk,
        )
        .unwrap()
}

#[test]
fn checkpoint_create_step_restart_and_cancel_are_durable() {
    let root = root("restart");
    let store = AlgoExecutionCheckpointStore::new(&root);
    let state = initial_state();
    store.create(&state).unwrap();
    assert!(store.create(&state).is_err(), "parent IDs cannot be reused");

    assert!(matches!(
        advance_once(store.clone()),
        AlgoExecutionStep::Submitted {
            client_order_id: 100,
            ..
        }
    ));
    let restarted = AlgoExecutionCheckpointStore::new(&root)
        .load(&state.parent_id)
        .unwrap();
    assert_eq!(restarted.next_slice_position, 1);
    assert_eq!(restarted.child_order_ids, [100]);

    let cancelled = store.request_cancel(&state.parent_id).unwrap();
    assert!(cancelled.cancel_requested);
    assert_eq!(
        AlgoExecutionCheckpointStore::new(&root)
            .load(&state.parent_id)
            .unwrap(),
        cancelled
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn two_workers_cannot_advance_the_same_parent_checkpoint_twice() {
    let root = root("single-flight");
    let store = AlgoExecutionCheckpointStore::new(&root);
    store.create(&initial_state()).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let workers = (0..2)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                advance_once(store)
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, AlgoExecutionStep::Submitted { .. }))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, AlgoExecutionStep::NotDue { .. }))
            .count(),
        1
    );
    let state = store.load("parent-恢复/with unsafe path chars").unwrap();
    assert_eq!(state.next_slice_position, 1);
    assert_eq!(state.child_order_ids, [100]);
    std::fs::remove_dir_all(root).unwrap();
}
