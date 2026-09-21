//! 三家 Venue（Paper / CCXT / Binance）共用的**订单回报契约** harness。
//!
//! V9 §8.3 第 8 项此前只覆盖到"提交侧未知结果"（`src/tests.rs` 的
//! `paper_ccxt_and_binance_share_unknown_submit_fact_contract`）。本文件补的是回报侧：
//!
//! 1. **迟到/乱序回报**：终态订单不能被后续回报改写（Fill 之后迟到的撤单、Cancel 之后
//!    迟到的成交、已按更大数量记账后到达的旧累计回报），重复推送必须幂等；
//! 2. **精度越界回报**：落在冻结产品规格 tick/step 之外的成交不得记账，必须 fail-closed
//!    转为待对账事实。
//!
//! 三家共用同一段断言代码（`assert_venue_report_contract`），差异只体现在各自的
//! `VenueReportHarness` 实现里：Paper 走 `on_quote`/`cancel`，CCXT 走脚本化 RPC +
//! `sync_order`，Binance 走 `executionReport` 用户流 JSON。事实一律经
//! `ingest_venue_events_with_spec` 归约进**真实** `LiveEventPipeline`（OMS + Ledger +
//! EventLog），因此这里断言的是可观察的记账结果，而不是某个端口的形状。

use qx_adapter::{
    BinanceSpotAuth, BinanceSpotVenue, CcxtProcessVenue, CcxtRpc, HttpRequest, HttpResponse,
    HttpTransport,
};
use qx_core::{
    AccountCashflow, CashflowKind, EventKind, InstrumentId, Money, Order, OrderStatus, Price,
    Quantity, Side, TradingInstrumentSpec, TradingProduct, SCALE,
};
use qx_execution::{ingest_venue_events_with_spec, EventLogReconcilePort};
use qx_execution::{OrderStore, ReconcilePort};
use qx_guanxing::QuoteTick;
use qx_runtime::{LiveEventPipeline, RuntimeEventEnvelope, RuntimeExternalEvent};
use qx_zhenlu::{PaperVenue, Venue, VenueEvent};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// 0.1（价格 tick 与数量步长），定点 SCALE。
const STEP: i128 = SCALE / 10;
/// 100 USDT。
const PRICE: i128 = 100 * SCALE;
/// 100.05：不在 0.1 的 tick 上。
const OFF_TICK_PRICE: i128 = PRICE + SCALE / 20;
/// 0.15：不在 0.1 的 step 上。
const OFF_STEP_QTY: i128 = STEP + SCALE / 20;
/// 1.0 个币（订单数量）。
const ORDER_QTY: i128 = SCALE;
/// 迟到回报的业务时间必须早于已记账的回报。
const LATE_TS: u64 = 1_500;
const TS: u64 = 2_000;
const RECEIVE_TS: u64 = 2_100;

fn instrument() -> InstrumentId {
    InstrumentId::parse("BTC/USDT.BINANCE").unwrap()
}

fn spec() -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument: instrument(),
        product: TradingProduct::Spot,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: true,
        inverse: false,
        price_tick: STEP,
        qty_step: STEP,
        min_qty: STEP,
        max_leverage: 1,
        maintenance_margin_bps: 0,
        valid_from: 0,
        valid_to: None,
    }
}

fn order(client_id: u64, qty: i128, limit: i128, status: OrderStatus) -> Order {
    Order {
        client_id,
        instrument: instrument(),
        side: Side::Buy,
        qty: Quantity::from_raw(qty),
        limit: Some(Price::from_raw(limit)),
        status,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
    }
}

/// 定点原始值 -> 交易所字符串（`100`、`100.05`）。
fn decimal(raw: i128) -> String {
    let sign = if raw < 0 { "-" } else { "" };
    let abs = raw.abs();
    let whole = abs / SCALE;
    let frac = abs % SCALE;
    if frac == 0 {
        return format!("{sign}{whole}");
    }
    let frac = format!("{frac:09}").trim_end_matches('0').to_string();
    format!("{sign}{whole}.{frac}")
}

/// 三家共用的真实事件管线，以及可观察记账结果的读取口。
struct Contract {
    pipeline: LiveEventPipeline,
    root: PathBuf,
    seq: u64,
}

impl Contract {
    fn open(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "qianxing-venue-report-{label}-{}-{nanos}",
            std::process::id()
        ));
        let mut contract = Self {
            pipeline: LiveEventPipeline::open(&root, "events", "USDT").unwrap(),
            root,
            seq: 0,
        };
        // 现货买入需要现金；不预置资金会让记账因余额不足而失败，掩盖真正的回报契约。
        contract
            .pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountCashflow {
                    cashflow: AccountCashflow {
                        account_id: "main".into(),
                        venue_id: "contract".into(),
                        currency: "USDT".into(),
                        kind: CashflowKind::Transfer,
                        amount: Money::from_i64(10_000),
                        external_id: "contract-deposit".into(),
                    },
                },
                1,
                1,
                0,
                "contract:deposit",
            ))
            .unwrap();
        contract
    }

    fn cleanup(self) {
        let _ = std::fs::remove_dir_all(self.root);
    }

    fn register(&mut self, order: &Order) {
        <LiveEventPipeline as OrderStore>::register_order(
            &mut self.pipeline,
            order.clone(),
            TS,
            Some(format!("contract:register:{}", order.client_id)),
        )
        .unwrap();
    }

    fn ingest(&mut self, events: Vec<VenueEvent>) -> Result<usize, String> {
        let frozen = spec();
        ingest_venue_events_with_spec(
            &mut self.pipeline,
            events,
            "contract",
            RECEIVE_TS,
            &mut self.seq,
            &frozen,
        )
    }

    /// 与生产 worker 一致：回报通道返回错误时，只能留下一条待对账事实。
    fn require_reconcile(&mut self, client_order_id: u64, reason: &str) {
        EventLogReconcilePort::new(&mut self.pipeline, "contract", RECEIVE_TS, &mut self.seq)
            .with_tag("report-error")
            .require_reconcile(client_order_id, reason)
            .unwrap();
    }

    fn events(&self) -> Vec<EventKind> {
        self.pipeline
            .log()
            .events()
            .iter()
            .map(|event| event.kind.clone())
            .collect()
    }

    fn count(&self, id: u64, kind: &str) -> usize {
        self.events()
            .iter()
            .filter(|event| match (event, kind) {
                (EventKind::Filled { fill }, "fill") => fill.order_id == id,
                (
                    EventKind::Accepted {
                        client_order_id, ..
                    },
                    "accepted",
                ) => *client_order_id == id,
                (EventKind::Cancelled { client_order_id }, "cancelled") => *client_order_id == id,
                (EventKind::ReconcileRequired { client_order_id }, "reconcile") => {
                    *client_order_id == id
                }
                (EventKind::LedgerApplied { entry }, "ledger") => entry.order_id == Some(id),
                _ => false,
            })
            .count()
    }

    fn ledger(&self, id: u64) -> usize {
        self.count(id, "ledger")
    }

    fn status(&self, id: u64) -> OrderStatus {
        self.pipeline
            .orders()
            .iter()
            .find(|order| order.client_id == id)
            .map(|order| order.status)
            .unwrap()
    }

    fn filled(&self, id: u64) -> i128 {
        self.pipeline
            .orders()
            .iter()
            .find(|order| order.client_id == id)
            .map(|order| order.filled.raw())
            .unwrap()
    }
}

/// 回报通道返回的事实里是否含有某一类事实。
fn has_fill(events: &[VenueEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, VenueEvent::Fill(_)))
}

fn has_cancel(events: &[VenueEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, VenueEvent::Cancelled { .. }))
}

/// 一家 Venue 的回报通道。实现者只负责"把中立回报翻译成该交易所的真实回报形状"，
/// 所有断言都在 `assert_venue_report_contract` 里共用。
trait VenueReportHarness {
    fn name(&self) -> &'static str;
    fn contract(&mut self) -> &mut Contract;
    fn current_order(&self) -> u64;

    /// 开出订单：登记到 EventLog，并让 Venue 通过自己的回报通道回报受理。
    fn open_order(
        &mut self,
        client_id: u64,
        qty: i128,
        limit: i128,
    ) -> Result<Vec<VenueEvent>, String>;

    /// 报出一笔 (qty, price) 成交。
    fn report_fill(&mut self, qty: i128, price: i128) -> Result<Vec<VenueEvent>, String>;
    /// 把上一条成交回报原样再送一次（WS 重放 / REST 轮询重复）。
    fn replay_last_fill(&mut self) -> Result<Vec<VenueEvent>, String>;
    /// 送一条业务时间更早、累计量已被后续回报覆盖的成交回报。
    fn report_stale_fill(&mut self) -> Result<Vec<VenueEvent>, String>;
    /// 报出撤单成功。
    fn report_cancel(&mut self) -> Result<Vec<VenueEvent>, String>;
}

/// 正常回报必须成功落地；任何"拒绝"都是契约违例。
fn deliver_ok<H: VenueReportHarness>(
    h: &mut H,
    stage: &str,
    report: impl FnOnce(&mut H) -> Result<Vec<VenueEvent>, String>,
) {
    let venue = h.name();
    let events = report(h)
        .unwrap_or_else(|error| panic!("{venue} 在 {stage} 阶段不应拒绝合法回报: {error}"));
    h.contract()
        .ingest(events)
        .unwrap_or_else(|error| panic!("{venue} 在 {stage} 阶段归约回报失败: {error}"));
}

/// 终态之后的迟到回报：要么 Venue 直接拒绝（此时必须留下待对账事实），要么不产生该类事实；
/// 但绝不允许把成交/撤单事实记进管线。
fn deliver_after_terminal<H: VenueReportHarness>(
    h: &mut H,
    fills_expected: bool,
    report: impl FnOnce(&mut H) -> Result<Vec<VenueEvent>, String>,
) {
    let (venue, id) = (h.name(), h.current_order());
    let reason = match report(h) {
        Ok(events) => {
            let leaked = if fills_expected {
                has_fill(&events)
            } else {
                has_cancel(&events)
            };
            assert!(
                !leaked,
                "{venue}: 终态订单 {id} 仍被迟到的同类回报生成了新事实: {events:?}"
            );
            h.contract().ingest(events).unwrap_or_else(|error| {
                panic!("{venue}: 终态订单 {id} 的迟到回报归约失败: {error}")
            });
            return;
        }
        Err(error) => error,
    };
    // 拒绝必须落在"结果未知"这一类：Invariant/Permanent 会被 worker 当硬错误处理，
    // 迟到回报对应的订单就永远进不了对账队列。
    assert!(
        reason.contains("ReconcileRequired"),
        "{venue}: 终态订单 {id} 的迟到回报被拒绝为 {reason}，未分类为待对账"
    );
    // 真实交易所的"本地已终态、远端仍有动作"只能交给对账。
    h.contract().require_reconcile(id, &reason);
}

/// 迟到的旧累计回报：允许 Venue 拒绝（则留下待对账事实），也允许它什么都不产生；
/// 已记账数量是否被改写由调用方断言。
fn deliver_lenient<H: VenueReportHarness>(
    h: &mut H,
    report: impl FnOnce(&mut H) -> Result<Vec<VenueEvent>, String>,
) {
    let id = h.current_order();
    match report(h) {
        Ok(events) => {
            let _ = h.contract().ingest(events);
        }
        Err(error) => h.contract().require_reconcile(id, &error),
    }
}

/// 送出一笔精度越界的成交回报，返回"Venue 是否确实生成了成交事实"与归约结果。
fn deliver_precision<H: VenueReportHarness>(
    h: &mut H,
    qty: i128,
    price: i128,
) -> (bool, Result<usize, String>) {
    let venue = h.name();
    let events = h.report_fill(qty, price).unwrap_or_else(|error| {
        panic!("{venue} 不应在通道侧判定 {qty}@{price} 是否合规（规格校验属于归约边界）: {error}")
    });
    let had_fill = has_fill(&events);
    (had_fill, h.contract().ingest(events))
}

/// 三家共用的回报契约。
fn assert_venue_report_contract<H: VenueReportHarness>(h: &mut H) {
    // ── 1. 基线成交 + 重复推送幂等 ─────────────────────────────────────────
    deliver_ok(h, "受理", |h| h.open_order(101, ORDER_QTY, PRICE));
    deliver_ok(h, "成交", |h| h.report_fill(ORDER_QTY, PRICE));
    let id = h.current_order();
    assert_eq!(
        h.contract().filled(id),
        ORDER_QTY,
        "{} 基线成交未记账",
        h.name()
    );
    assert_eq!(h.contract().status(id), OrderStatus::Filled);
    assert_eq!(h.contract().count(id, "fill"), 1);
    assert!(
        h.contract().ledger(id) > 0,
        "{} 基线成交必须产生账务条目",
        h.name()
    );
    let ledger_baseline = h.contract().ledger(id);
    let venue = h.name();
    deliver_ok(h, "重复推送", |h| h.replay_last_fill());
    assert_eq!(
        h.contract().count(id, "fill"),
        1,
        "{venue} 重复推送生成了第二条成交事实"
    );
    assert_eq!(
        h.contract().ledger(id),
        ledger_baseline,
        "{venue} 重复推送重复记账"
    );
    assert_eq!(h.contract().filled(id), ORDER_QTY);

    // ── 2. 乱序：更早的累计回报后到，已记账数量不得回退 ───────────────────
    deliver_ok(h, "受理", |h| h.open_order(102, ORDER_QTY, PRICE));
    let id = h.current_order();
    let partial = ORDER_QTY / 10 * 4;
    deliver_ok(h, "部分成交", |h| h.report_fill(partial, PRICE));
    assert_eq!(h.contract().filled(id), partial);
    assert_eq!(h.contract().status(id), OrderStatus::PartiallyFilled);
    deliver_ok(h, "续成交", |h| {
        h.report_fill(ORDER_QTY - partial, PRICE)
    });
    assert_eq!(h.contract().status(id), OrderStatus::Filled);
    // 覆盖式旧回报（累计量小于已记账量，或同一 trade 重放）必须被拒绝或被忽略。
    deliver_lenient(h, |h| h.report_stale_fill());
    assert_eq!(
        h.contract().filled(id),
        ORDER_QTY,
        "{}: 迟到的旧回报把已记账数量改写了",
        h.name()
    );
    assert_eq!(
        h.contract().count(id, "fill"),
        2,
        "{}: 旧回报生成了新的成交事实",
        h.name()
    );

    // ── 3. Cancel 之后迟到的成交：永不记账 ─────────────────────────────────
    deliver_ok(h, "受理", |h| h.open_order(103, ORDER_QTY, PRICE));
    deliver_ok(h, "撤单", |h| h.report_cancel());
    let id = h.current_order();
    assert_eq!(h.contract().status(id), OrderStatus::Cancelled);
    let before = h.contract().ledger(id);
    deliver_after_terminal(h, true, |h| h.report_fill(partial / 2, PRICE));
    assert_eq!(
        h.contract().filled(id),
        0,
        "{}: 撤单后的迟到成交被记账了",
        h.name()
    );
    assert_eq!(
        h.contract().ledger(id),
        before,
        "{}: 撤单后的迟到成交产生了账务条目",
        h.name()
    );
    assert_eq!(h.contract().count(id, "fill"), 0);
    assert!(
        matches!(
            h.contract().status(id),
            OrderStatus::Cancelled | OrderStatus::Unknown
        ),
        "{}: 撤单后的迟到回报把订单改回了非终态: {:?}",
        h.name(),
        h.contract().status(id)
    );

    // ── 4. Fill 之后迟到的撤单：不得改写终态 ───────────────────────────────
    deliver_ok(h, "受理", |h| h.open_order(104, ORDER_QTY, PRICE));
    let id = h.current_order();
    deliver_ok(h, "成交", |h| h.report_fill(ORDER_QTY, PRICE));
    let ledger_baseline = h.contract().ledger(id);
    deliver_after_terminal(h, false, |h| h.report_cancel());
    assert_eq!(
        h.contract().status(id),
        OrderStatus::Filled,
        "{}: 成交后迟到撤单改写了终态",
        h.name()
    );
    assert_eq!(h.contract().count(id, "cancelled"), 0);
    assert_eq!(h.contract().ledger(id), ledger_baseline);
    assert_eq!(h.contract().filled(id), ORDER_QTY);

    // ── 5. 精度越界回报：价格不在 tick 上 ──────────────────────────────────
    // 限价放宽到 110，让 Paper 也能在 100.05 这个 off-tick 价上成交（Paper 按对手价撮合）。
    let loose_limit = PRICE + 10 * SCALE;
    deliver_ok(h, "受理", |h| h.open_order(105, ORDER_QTY, loose_limit));
    let id = h.current_order();
    // 报满剩余量：越界回报被拒后三家 Venue 的本地副本都已终态，下一用例不会被上一张单干扰。
    let (had_fill, result) = deliver_precision(h, ORDER_QTY, OFF_TICK_PRICE);
    assert!(
        had_fill,
        "{}: off-tick 回报没有生成成交事实，本用例失效",
        h.name()
    );
    let error = result.expect_err("off-tick 成交回报必须 fail-closed，而不是静默记账");
    assert!(error.contains("精度越界"), "{}: {error}", h.name());
    assert_eq!(
        h.contract().count(id, "fill"),
        0,
        "off-tick 成交被记进了事实日志"
    );
    assert_eq!(h.contract().ledger(id), 0, "off-tick 成交产生了账务条目");
    assert_eq!(
        h.contract().count(id, "reconcile"),
        1,
        "off-tick 成交必须留下一条待对账事实"
    );
    assert_eq!(h.contract().status(id), OrderStatus::Unknown);

    // ── 6. 精度越界回报：数量不在 step 上 ─────────────────────────────────
    deliver_ok(h, "受理", |h| h.open_order(106, ORDER_QTY, loose_limit));
    let id = h.current_order();
    let (had_fill, result) = deliver_precision(h, OFF_STEP_QTY, PRICE);
    assert!(
        had_fill,
        "{}: off-step 回报没有生成成交事实，本用例失效",
        h.name()
    );
    let error = result.expect_err("off-step 成交回报必须 fail-closed");
    assert!(error.contains("精度越界"), "{}: {error}", h.name());
    assert_eq!(h.contract().count(id, "fill"), 0);
    assert_eq!(h.contract().ledger(id), 0);
    assert_eq!(h.contract().count(id, "reconcile"), 1);
    assert_eq!(h.contract().filled(id), 0);
}

// ─────────────────────────────── Paper ────────────────────────────────

struct PaperHarness {
    contract: Contract,
    venue: PaperVenue,
    current: u64,
    quote_seq: u64,
    last_quote: Option<QuoteTick>,
}

impl PaperHarness {
    fn new() -> Self {
        Self {
            contract: Contract::open("paper"),
            venue: PaperVenue::new("paper", zero_fee()),
            current: 0,
            quote_seq: 0,
            last_quote: None,
        }
    }
}

impl VenueReportHarness for PaperHarness {
    fn name(&self) -> &'static str {
        "Paper"
    }
    fn contract(&mut self) -> &mut Contract {
        &mut self.contract
    }
    fn current_order(&self) -> u64 {
        self.current
    }

    fn open_order(
        &mut self,
        client_id: u64,
        qty: i128,
        limit: i128,
    ) -> Result<Vec<VenueEvent>, String> {
        self.current = client_id;
        self.contract
            .register(&order(client_id, qty, limit, OrderStatus::PendingSubmit));
        let venue = self.venue.id().to_string();
        debug_assert_eq!(venue, "paper");
        self.venue
            .submit(order(client_id, qty, limit, OrderStatus::PendingSubmit), TS)
            .map_err(|error| format!("{error:?}"))
    }

    fn report_fill(&mut self, qty: i128, price: i128) -> Result<Vec<VenueEvent>, String> {
        self.quote_seq += 1;
        let quote = QuoteTick::new(
            TS + self.quote_seq,
            Price::from_raw(price),
            Quantity::from_raw(qty),
            Price::from_raw(price),
            Quantity::from_raw(qty),
            self.quote_seq,
        );
        self.last_quote = Some(quote);
        Ok(self.venue.on_quote(&instrument(), quote))
    }

    fn replay_last_fill(&mut self) -> Result<Vec<VenueEvent>, String> {
        let quote = self.last_quote.expect("replay 前必须有一笔成交回报");
        Ok(self.venue.on_quote(&instrument(), quote))
    }

    fn report_stale_fill(&mut self) -> Result<Vec<VenueEvent>, String> {
        // 迟到的旧报价：source_seq 已被更新，Paper 必须按序号单调把它丢掉。
        let mut quote = self.last_quote.expect("stale 前必须有一笔成交回报");
        quote.source_seq = 1;
        quote.ts = LATE_TS;
        Ok(self.venue.on_quote(&instrument(), quote))
    }

    fn report_cancel(&mut self) -> Result<Vec<VenueEvent>, String> {
        self.venue
            .cancel(self.current, TS)
            .map_err(|error| format!("{error:?}"))
    }
}

// ─────────────────────────────── CCXT ─────────────────────────────────

#[derive(Clone)]
struct RemoteOrder {
    order_id: String,
    status: String,
    filled: i128,
    cost: i128,
    ts: u64,
}

struct ScriptedCcxtRpc {
    remote: Arc<Mutex<RemoteOrder>>,
}

impl CcxtRpc for ScriptedCcxtRpc {
    fn call(&mut self, request: Value) -> Result<Value, String> {
        match request.get("op").and_then(Value::as_str) {
            Some("create_order") => {
                Ok(json!({"order": {"order_id": self.remote.lock().unwrap().order_id}}))
            }
            Some("cancel_order") => Ok(json!({"cancelled": true})),
            Some("fetch_my_trades") => Ok(json!({"trades": []})),
            Some("fetch_order") => {
                let remote = self.remote.lock().unwrap();
                Ok(json!({"order": {
                    "order_id": remote.order_id,
                    "status": remote.status,
                    "filled_raw": remote.filled,
                    "cost_raw": remote.cost,
                    "fee_raw": 0,
                    "timestamp_ms": remote.ts,
                }}))
            }
            Some(other) => Err(format!("脚本化 CCXT RPC 收到未预期操作: {other}")),
            None => Err("脚本化 CCXT RPC 请求缺少 op".into()),
        }
    }
}

struct CcxtHarness {
    contract: Contract,
    venue: CcxtProcessVenue,
    remote: Arc<Mutex<RemoteOrder>>,
    qty: i128,
    current: u64,
    /// 已推送过的累计快照，用于重放与乱序回放。
    history: Vec<RemoteOrder>,
}

impl CcxtHarness {
    fn new() -> Self {
        let remote = Arc::new(Mutex::new(RemoteOrder {
            order_id: "ccxt-remote-1".into(),
            status: "open".into(),
            filled: 0,
            cost: 0,
            ts: TS,
        }));
        let venue = CcxtProcessVenue::new(
            "ccxt-binance",
            Box::new(ScriptedCcxtRpc {
                remote: Arc::clone(&remote),
            }),
        );
        Self {
            contract: Contract::open("ccxt"),
            venue,
            remote,
            qty: 0,
            current: 0,
            history: Vec::new(),
        }
    }

    fn publish(&mut self, filled: i128, price: i128) -> Result<Vec<VenueEvent>, String> {
        let cost = filled * price / SCALE;
        let qty = self.qty;
        {
            let mut remote = self.remote.lock().unwrap();
            remote.filled = filled;
            remote.cost = cost;
            remote.status = if filled >= qty {
                "closed".into()
            } else if filled == 0 {
                "canceled".into()
            } else {
                "open".into()
            };
            let snapshot = remote.clone();
            self.history.push(snapshot);
        }
        self.venue
            .sync_order(self.current, TS)
            .map_err(|error| format!("{error:?}"))
    }
}

impl VenueReportHarness for CcxtHarness {
    fn name(&self) -> &'static str {
        "CCXT"
    }
    fn contract(&mut self) -> &mut Contract {
        &mut self.contract
    }
    fn current_order(&self) -> u64 {
        self.current
    }

    fn open_order(
        &mut self,
        client_id: u64,
        qty: i128,
        limit: i128,
    ) -> Result<Vec<VenueEvent>, String> {
        self.current = client_id;
        self.qty = qty;
        self.history.clear();
        {
            let mut remote = self.remote.lock().unwrap();
            remote.order_id = format!("ccxt-remote-{client_id}");
            remote.status = "open".into();
            remote.filled = 0;
            remote.cost = 0;
        }
        self.contract
            .register(&order(client_id, qty, limit, OrderStatus::PendingSubmit));
        self.venue
            .submit(order(client_id, qty, limit, OrderStatus::PendingSubmit), TS)
            .map_err(|error| format!("{error:?}"))
    }

    fn report_fill(&mut self, qty: i128, price: i128) -> Result<Vec<VenueEvent>, String> {
        let filled = self
            .remote
            .lock()
            .unwrap()
            .filled
            .checked_add(qty)
            .expect("累计成交溢出");
        self.publish(filled, price)
    }

    fn replay_last_fill(&mut self) -> Result<Vec<VenueEvent>, String> {
        let last = self
            .history
            .last()
            .expect("replay 前必须有一笔回报")
            .clone();
        {
            let mut remote = self.remote.lock().unwrap();
            *remote = last;
        }
        self.venue
            .sync_order(self.current, TS)
            .map_err(|error| format!("{error:?}"))
    }

    fn report_stale_fill(&mut self) -> Result<Vec<VenueEvent>, String> {
        // 交易所推来一条更早的累计快照（first-trade 视图回退），必须拒绝而不是回退记账。
        let stale = self
            .history
            .first()
            .expect("stale 前必须有一笔回报")
            .clone();
        {
            let mut remote = self.remote.lock().unwrap();
            *remote = stale;
        }
        self.venue
            .sync_order(self.current, TS)
            .map_err(|error| format!("{error:?}"))
    }

    fn report_cancel(&mut self) -> Result<Vec<VenueEvent>, String> {
        self.venue
            .cancel(self.current, TS)
            .map_err(|error| format!("{error:?}"))
    }
}

// ─────────────────────────────── Binance ──────────────────────────────

/// 用户流契约 harness 只走 WS 回报，绝不发 REST；调用即视为契约违例。
struct NoRestTransport;

impl HttpTransport for NoRestTransport {
    fn send(&self, _request: HttpRequest) -> Result<HttpResponse, String> {
        Err("回报契约 harness 不允许发起 REST 请求".into())
    }
}

struct BinanceHarness {
    contract: Contract,
    venue: BinanceSpotVenue,
    qty: i128,
    current: u64,
    trade_seq: u64,
    filled: i128,
    /// 已送出的报文原文，用于重放与迟到回放。
    history: Vec<String>,
}

impl BinanceHarness {
    fn new() -> Self {
        let auth = BinanceSpotAuth::with_clock("key", b"secret", || 100).unwrap();
        let venue = BinanceSpotVenue::with_endpoint(
            "binance",
            auth,
            Arc::new(NoRestTransport),
            "mock.binance",
            443,
        );
        Self {
            contract: Contract::open("binance"),
            venue,
            qty: 0,
            current: 0,
            trade_seq: 0,
            filled: 0,
            history: Vec::new(),
        }
    }

    fn trade_payload(&mut self, qty: i128, price: i128, ts: u64) -> String {
        self.trade_seq += 1;
        self.filled += qty;
        let status = if self.filled >= self.qty {
            "FILLED"
        } else {
            "PARTIALLY_FILLED"
        };
        json!({
            "e": "executionReport",
            "E": ts,
            "T": ts,
            "c": format!("qx-{}", self.current),
            "i": 90_000 + self.current,
            "x": "TRADE",
            "X": status,
            "L": decimal(price),
            "l": decimal(qty),
            "n": "0",
            "N": "USDT",
            "t": self.trade_seq,
        })
        .to_string()
    }

    fn deliver(&mut self, payload: &str) -> Result<Vec<VenueEvent>, String> {
        self.venue
            .ingest_user_event(payload)
            .map_err(|error| format!("{error:?}"))
    }
}

impl VenueReportHarness for BinanceHarness {
    fn name(&self) -> &'static str {
        "Binance"
    }
    fn contract(&mut self) -> &mut Contract {
        &mut self.contract
    }
    fn current_order(&self) -> u64 {
        self.current
    }

    fn open_order(
        &mut self,
        client_id: u64,
        qty: i128,
        limit: i128,
    ) -> Result<Vec<VenueEvent>, String> {
        self.current = client_id;
        self.qty = qty;
        self.filled = 0;
        self.history.clear();
        // 用户流只带远端回报，因此必须先按 EventLog 恢复本地订单（与生产 worker 一致）。
        self.contract
            .register(&order(client_id, qty, limit, OrderStatus::PendingSubmit));
        self.venue
            .restore_orders([order(client_id, qty, limit, OrderStatus::Submitted)])
            .map_err(|error| format!("{error:?}"))?;
        let payload = json!({
            "e": "executionReport",
            "E": TS,
            "T": TS,
            "c": format!("qx-{client_id}"),
            "i": 90_000 + client_id,
            "x": "NEW",
            "X": "NEW",
        })
        .to_string();
        let events = self.deliver(&payload)?;
        self.history.push(payload);
        Ok(events)
    }

    fn report_fill(&mut self, qty: i128, price: i128) -> Result<Vec<VenueEvent>, String> {
        let payload = self.trade_payload(qty, price, TS);
        let events = self.deliver(&payload)?;
        self.history.push(payload);
        Ok(events)
    }

    fn replay_last_fill(&mut self) -> Result<Vec<VenueEvent>, String> {
        let last = self
            .history
            .last()
            .expect("replay 前必须有一笔回报")
            .clone();
        self.deliver(&last)
    }

    fn report_stale_fill(&mut self) -> Result<Vec<VenueEvent>, String> {
        let stale = self
            .history
            .first()
            .expect("stale 前必须有一笔回报")
            .clone();
        self.deliver(&stale)
    }

    fn report_cancel(&mut self) -> Result<Vec<VenueEvent>, String> {
        let payload = json!({
            "e": "executionReport",
            "E": TS,
            "T": TS,
            "c": format!("qx-{}", self.current),
            "i": 90_000 + self.current,
            "x": "CANCELED",
            "X": "CANCELED",
        })
        .to_string();
        let events = self.deliver(&payload)?;
        self.history.push(payload);
        Ok(events)
    }
}

/// 只断言回报形状 / 恢复事实的用例显式零费：费用不是这里的期望，但生产 Paper
/// 路径的成本口径必须由调用方给出（见 `PaperVenue::new`）。
fn zero_fee() -> Box<dyn qx_core::FeeModel + Send> {
    Box::new(qx_core::ZeroFeeModel)
}

#[test]
fn paper_ccxt_and_binance_share_late_and_imprecise_report_contract() {
    let mut paper = PaperHarness::new();
    assert_venue_report_contract(&mut paper);
    paper.contract.cleanup();

    let mut ccxt = CcxtHarness::new();
    assert_venue_report_contract(&mut ccxt);
    ccxt.contract.cleanup();

    let mut binance = BinanceHarness::new();
    assert_venue_report_contract(&mut binance);
    binance.contract.cleanup();
}
