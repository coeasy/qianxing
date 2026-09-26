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

mod harness;
pub(crate) use harness::*;
mod venues;
