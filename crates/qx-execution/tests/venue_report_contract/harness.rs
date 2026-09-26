use super::*;

/// 0.1（价格 tick 与数量步长），定点 SCALE。
pub(crate) const STEP: i128 = SCALE / 10;
/// 100 USDT。
pub(crate) const PRICE: i128 = 100 * SCALE;
/// 100.05：不在 0.1 的 tick 上。
pub(crate) const OFF_TICK_PRICE: i128 = PRICE + SCALE / 20;
/// 0.15：不在 0.1 的 step 上。
pub(crate) const OFF_STEP_QTY: i128 = STEP + SCALE / 20;
/// 1.0 个币（订单数量）。
pub(crate) const ORDER_QTY: i128 = SCALE;
/// 迟到回报的业务时间必须早于已记账的回报。
pub(crate) const LATE_TS: u64 = 1_500;
pub(crate) const TS: u64 = 2_000;
pub(crate) const RECEIVE_TS: u64 = 2_100;

pub(crate) fn instrument() -> InstrumentId {
    InstrumentId::parse("BTC/USDT.BINANCE").unwrap()
}

pub(crate) fn spec() -> TradingInstrumentSpec {
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

pub(crate) fn order(client_id: u64, qty: i128, limit: i128, status: OrderStatus) -> Order {
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
pub(crate) fn decimal(raw: i128) -> String {
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
pub(crate) struct Contract {
    pipeline: LiveEventPipeline,
    root: PathBuf,
    seq: u64,
}

impl Contract {
    pub(crate) fn open(label: &str) -> Self {
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

    pub(crate) fn cleanup(self) {
        let _ = std::fs::remove_dir_all(self.root);
    }

    pub(crate) fn register(&mut self, order: &Order) {
        <LiveEventPipeline as OrderStore>::register_order(
            &mut self.pipeline,
            order.clone(),
            TS,
            Some(format!("contract:register:{}", order.client_id)),
        )
        .unwrap();
    }

    pub(crate) fn ingest(&mut self, events: Vec<VenueEvent>) -> Result<usize, String> {
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
    pub(crate) fn require_reconcile(&mut self, client_order_id: u64, reason: &str) {
        EventLogReconcilePort::new(&mut self.pipeline, "contract", RECEIVE_TS, &mut self.seq)
            .with_tag("report-error")
            .require_reconcile(client_order_id, reason)
            .unwrap();
    }

    pub(crate) fn events(&self) -> Vec<EventKind> {
        self.pipeline
            .log()
            .events()
            .iter()
            .map(|event| event.kind.clone())
            .collect()
    }

    pub(crate) fn count(&self, id: u64, kind: &str) -> usize {
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

    pub(crate) fn ledger(&self, id: u64) -> usize {
        self.count(id, "ledger")
    }

    pub(crate) fn status(&self, id: u64) -> OrderStatus {
        self.pipeline
            .orders()
            .iter()
            .find(|order| order.client_id == id)
            .map(|order| order.status)
            .unwrap()
    }

    pub(crate) fn filled(&self, id: u64) -> i128 {
        self.pipeline
            .orders()
            .iter()
            .find(|order| order.client_id == id)
            .map(|order| order.filled.raw())
            .unwrap()
    }
}

/// 回报通道返回的事实里是否含有某一类事实。
pub(crate) fn has_fill(events: &[VenueEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, VenueEvent::Fill(_)))
}

pub(crate) fn has_cancel(events: &[VenueEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, VenueEvent::Cancelled { .. }))
}

/// 一家 Venue 的回报通道。实现者只负责"把中立回报翻译成该交易所的真实回报形状"，
/// 所有断言都在 `assert_venue_report_contract` 里共用。
pub(crate) trait VenueReportHarness {
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
pub(crate) fn deliver_ok<H: VenueReportHarness>(
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
pub(crate) fn deliver_after_terminal<H: VenueReportHarness>(
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
pub(crate) fn deliver_lenient<H: VenueReportHarness>(
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
pub(crate) fn deliver_precision<H: VenueReportHarness>(
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
pub(crate) fn assert_venue_report_contract<H: VenueReportHarness>(h: &mut H) {
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
