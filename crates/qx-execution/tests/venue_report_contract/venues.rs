use super::*;

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
