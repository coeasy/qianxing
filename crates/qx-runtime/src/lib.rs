//! 牵星运行时装配边界。
//!
//! 该 crate 不拥有交易事实，也不替代 Kernel；它只负责把 API、行情、用户流、
//! 调度和对账 worker 的配置校验成一个可审计的进程拓扑，并提供确定性的健康状态
//! 与停机信号。具体 worker 通过此边界注入，不允许把凭证或可变账户状态写入配置摘要。

use qx_control::Permission;
use qx_core::{InstrumentId, MarginMode, PositionMode, TradingProduct};
use qx_storage::JsonStateStore;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

mod data_binding;
mod pipeline;
mod runtime_config;
mod strategy_contract;
mod supervision;
mod worker_policy;

pub use data_binding::{RuntimeDatasetBinding, RuntimeResearchBinding};
pub use pipeline::{
    order_from_submit_command, pipeline_path, LiveEventPipeline, LivePipelineSnapshot,
    PipelineMetricsSnapshot, RuntimeBalanceDiscrepancy, RuntimeEventEnvelope, RuntimeExternalEvent,
    RuntimeIngestReceipt,
};
pub use runtime_config::*;
pub use strategy_contract::*;
pub use supervision::*;
pub use worker_policy::{FieldScope, RoleFieldStatus, WorkerRoleFieldScopes, ALL_WORKER_ROLES};

/// 将共享 EventLog 运行时适配为应用层执行端口。
///
/// 适配器只负责把应用层的稳定执行事实映射为 Runtime 事件；订单注册仍由
/// `LiveEventPipeline` 统一完成校验、幂等和日志追加，避免应用层绕过 Kernel。
impl qx_execution::OrderStore for LiveEventPipeline {
    fn orders(&self) -> Vec<qx_core::Order> {
        LiveEventPipeline::orders(self)
    }

    fn register_order(
        &mut self,
        order: qx_core::Order,
        ts: u64,
        correlation_id: Option<String>,
    ) -> Result<(), String> {
        self.register_order_with_correlation(order, ts, correlation_id)
            .map(|_| ())
            .map_err(|error| format!("注册订单失败: {error:?}"))
    }
}

impl qx_execution::EventAppender for LiveEventPipeline {
    fn append_execution_event(
        &mut self,
        envelope: qx_execution::ExecutionEventEnvelope,
    ) -> Result<(), String> {
        let receipt = match envelope.event {
            qx_execution::ExecutionEvent::MarketQuote { instrument, quote } => {
                self.ingest(RuntimeEventEnvelope::market_quote(
                    instrument,
                    quote,
                    envelope.event_ts,
                    envelope.source_seq,
                    envelope.correlation_id,
                ))
            }
            event => {
                let event = match event {
                    qx_execution::ExecutionEvent::Accepted {
                        client_order_id,
                        venue_order_id,
                    } => RuntimeExternalEvent::Accepted {
                        client_order_id,
                        venue_order_id: Some(venue_order_id),
                    },
                    qx_execution::ExecutionEvent::Fill(fill) => {
                        RuntimeExternalEvent::Fill { fill: *fill }
                    }
                    qx_execution::ExecutionEvent::FillWithSpec { fill, spec } => {
                        RuntimeExternalEvent::FillWithSpec { fill, spec }
                    }
                    qx_execution::ExecutionEvent::Cancelled { client_order_id } => {
                        RuntimeExternalEvent::Cancelled { client_order_id }
                    }
                    qx_execution::ExecutionEvent::ReconcileRequired { client_order_id } => {
                        RuntimeExternalEvent::ReconcileRequired { client_order_id }
                    }
                    qx_execution::ExecutionEvent::MarketQuote { .. } => {
                        unreachable!("MarketQuote 在上面的分支处理")
                    }
                };
                self.ingest(RuntimeEventEnvelope::venue(
                    event,
                    envelope.event_ts,
                    envelope.receive_ts,
                    envelope.source_seq,
                    envelope.correlation_id,
                ))
            }
        };
        receipt
            .map(|_| ())
            .map_err(|error| format!("执行事实归约失败: {error:?}"))
    }
}

impl qx_execution::LedgerProbe for LiveEventPipeline {
    fn ledger_entry_count(&self) -> usize {
        self.ledger().entries().len()
    }
}

impl qx_execution::MarketDataPort for LiveEventPipeline {
    fn latest_quote(&self, instrument: &InstrumentId) -> Option<qx_guanxing::QuoteTick> {
        self.latest_quote_with_depth(instrument)
    }
}

/// 加载控制面状态；首次启动返回空状态，损坏的 JSON 不会被吞掉。
pub fn load_control_state(
    root: impl Into<std::path::PathBuf>,
) -> Result<qx_control::ControlPlane, String> {
    JsonStateStore::new(root)
        .load_control_if_exists()
        .map_err(|error| format!("读取控制面状态失败: {error:?}"))
        .map(|state| state.unwrap_or_default())
}

pub fn save_control_state(
    root: impl Into<std::path::PathBuf>,
    state: &qx_control::ControlPlane,
) -> Result<(), String> {
    JsonStateStore::new(root)
        .save_control(state)
        .map(|_| ())
        .map_err(|error| format!("保存控制面状态失败: {error:?}"))
}
