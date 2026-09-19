//! `ReconcilePort` 契约测试。
//!
//! “结果未知，必须先对账”这一事实在 Paper（真实 EventLog 管线）与 CCXT worker
//! （最小内存端口）两条链路上必须由同一个适配器 `EventLogReconcilePort` 产生完全
//! 一致的可观察结果：序号独占推进、correlation 口径、订单置为 Unknown、且绝不
//! 触碰 Ledger。

use qx_application::{
    EventAppender, ExecutionEvent, ExecutionEventEnvelope, OrderStore, ReconcilePort,
};
use qx_core::{InstrumentId, Order, OrderStatus, Price, Quantity, Side};
use qx_execution::EventLogReconcilePort;
use qx_runtime::LiveEventPipeline;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

/// 一条待对账事实的可观察部分（订单号、业务时间、接收时间、来源序号、审计关联）。
type ReconcileFact = (u64, u64, u64, u64, String);

trait ReconcileHarness: EventAppender {
    fn prepare_order(&mut self, client_id: u64);
    fn reconcile_facts(&self) -> Vec<ReconcileFact>;
    fn marks_unknown(&self, client_id: u64) -> bool;
    fn ledger_entry_count(&self) -> usize;
}

impl ReconcileHarness for LiveEventPipeline {
    fn prepare_order(&mut self, client_id: u64) {
        let order = Order {
            client_id,
            instrument: InstrumentId::parse("BTC/USDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(1),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        };
        // 走应用层端口注册，与 worker 生产路径一致（带 correlation 审计）。
        <Self as OrderStore>::register_order(self, order, 1, Some("contract-order".into()))
            .unwrap();
    }

    fn reconcile_facts(&self) -> Vec<ReconcileFact> {
        self.log()
            .events()
            .iter()
            .filter_map(|event| match &event.kind {
                qx_core::EventKind::ReconcileRequired { client_order_id } => Some((
                    *client_order_id,
                    event.ts,
                    event.receive_time,
                    event.source_seq,
                    event.correlation_id.clone(),
                )),
                _ => None,
            })
            .collect()
    }

    fn marks_unknown(&self, client_id: u64) -> bool {
        self.orders()
            .iter()
            .any(|order| order.client_id == client_id && order.status == OrderStatus::Unknown)
    }

    fn ledger_entry_count(&self) -> usize {
        self.ledger().entries().len()
    }
}

/// CCXT worker 侧的最小端口：只保留事实，不产生 Ledger 副作用。
#[derive(Default)]
struct RecordingAppender {
    events: Vec<ExecutionEventEnvelope>,
    reconciled: BTreeSet<u64>,
    orders: BTreeSet<u64>,
}

impl EventAppender for RecordingAppender {
    fn append_execution_event(&mut self, envelope: ExecutionEventEnvelope) -> Result<(), String> {
        if let ExecutionEvent::ReconcileRequired { client_order_id } = &envelope.event {
            self.reconciled.insert(*client_order_id);
        }
        self.events.push(envelope);
        Ok(())
    }
}

impl ReconcileHarness for RecordingAppender {
    fn prepare_order(&mut self, client_id: u64) {
        self.orders.insert(client_id);
    }

    fn reconcile_facts(&self) -> Vec<ReconcileFact> {
        self.events
            .iter()
            .filter_map(|envelope| match &envelope.event {
                ExecutionEvent::ReconcileRequired { client_order_id } => Some((
                    *client_order_id,
                    envelope.event_ts,
                    envelope.receive_ts,
                    envelope.source_seq,
                    envelope.correlation_id.clone(),
                )),
                _ => None,
            })
            .collect()
    }

    fn marks_unknown(&self, client_id: u64) -> bool {
        // 内存端口把订单状态归约委托给真实 OMS，这里只确认事实已经落账。
        self.reconciled.contains(&client_id) && self.orders.contains(&client_id)
    }

    fn ledger_entry_count(&self) -> usize {
        0
    }
}

fn assert_reconcile_port_contract<H: ReconcileHarness>(harness: &mut H) {
    harness.prepare_order(11);
    harness.prepare_order(12);
    let mut source_seq = 7_u64;

    // 缺少原因的待对账请求必须被拒绝：连原因都没有的“未知结果”无法审计。
    let rejected = EventLogReconcilePort::new(harness, "worker-a", 100, &mut source_seq)
        .require_reconcile(11, "   ")
        .unwrap_err();
    assert!(rejected.contains("缺少未知结果的原因"), "{rejected}");
    assert_eq!(source_seq, 7);
    assert!(harness.reconcile_facts().is_empty());

    // 默认口径：correlation 为 `<worker>:reconcile:<id>`，业务时间等于接收时间。
    EventLogReconcilePort::new(harness, "worker-a", 100, &mut source_seq)
        .require_reconcile(11, "REST 提交超时")
        .unwrap();
    assert_eq!(
        harness.reconcile_facts(),
        vec![(11, 100, 100, 8, "worker-a:reconcile:11".to_string())]
    );
    assert_eq!(source_seq, 8);
    assert!(harness.marks_unknown(11));

    // worker 可用 `at_event_ts` 保留远端回报时间，用 `with_tag` 保留各自审计口径。
    EventLogReconcilePort::new(harness, "worker-a", 121, &mut source_seq)
        .at_event_ts(120)
        .with_tag("sync-error")
        .require_reconcile(12, "回报缺少 orderId")
        .unwrap();
    assert_eq!(
        harness.reconcile_facts()[1],
        (12, 120, 121, 9, "worker-a:sync-error:12".to_string())
    );
    assert_eq!(source_seq, 9);
    assert!(harness.marks_unknown(12));

    // 待对账事实只改订单状态，绝不产生账务条目。
    assert_eq!(harness.ledger_entry_count(), 0);
}

#[test]
fn reconcile_port_contract_holds_on_paper_event_log() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-reconcile-port-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut pipeline = LiveEventPipeline::open(&root, "events", "USDT").unwrap();
    assert_reconcile_port_contract(&mut pipeline);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn reconcile_port_contract_holds_on_ccxt_worker_port() {
    let mut appender = RecordingAppender::default();
    assert_reconcile_port_contract(&mut appender);
}

/// 写入失败时端口必须把原因带回调用方，否则日志里只剩“未知结果”。
#[derive(Default)]
struct BrokenAppender {
    attempts: BTreeMap<u64, String>,
}

impl EventAppender for BrokenAppender {
    fn append_execution_event(&mut self, envelope: ExecutionEventEnvelope) -> Result<(), String> {
        match &envelope.event {
            ExecutionEvent::ReconcileRequired { .. } => {}
            _ => return Err("unexpected event".into()),
        }
        self.attempts
            .insert(envelope.source_seq, envelope.correlation_id.clone());
        Err("共享 EventLog 已被其他 worker 追加".into())
    }
}

#[test]
fn reconcile_port_reports_reason_when_append_fails() {
    let mut harness = BrokenAppender::default();
    let mut source_seq = 3_u64;
    let error = EventLogReconcilePort::new(&mut harness, "ccxt-pro", 500, &mut source_seq)
        .with_tag("order-error")
        .require_reconcile(21, "CCXT 订单查询返回 500")
        .unwrap_err();
    assert!(
        error.contains("共享 EventLog 已被其他 worker 追加"),
        "{error}"
    );
    assert!(
        error.contains("待对账原因: CCXT 订单查询返回 500"),
        "{error}"
    );
    // 失败的写入仍消耗序号，重试必须落在新的 EventLog 位置上。
    assert_eq!(source_seq, 4);
    assert_eq!(
        harness.attempts.get(&4).map(String::as_str),
        Some("ccxt-pro:order-error:21")
    );
}
