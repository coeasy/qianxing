//! Service hosting and process composition shared by command-line and daemon facades.
//!
//! This crate depends on runtime contracts and service adapters, and must never depend on a
//! facade such as `qx-cli`.

pub mod api_host;
pub mod api_read_model;
pub mod backtest_bindings;
pub mod binance_execution_host;
pub mod binance_reconcile_host;
pub mod binance_user_stream_host;
pub mod ccxt_execution_host;
pub mod ccxt_facts;
pub mod ccxt_live_bars;
pub mod ccxt_market_host;
pub mod ccxt_reconcile;
pub mod ccxt_reconcile_host;
pub mod ccxt_stream_retry;
pub mod ccxt_user_stream_host;
pub mod configured_backends;
pub mod http_host;
pub mod market_bridges;
#[cfg(feature = "nats")]
pub mod messaging_host;
pub mod one_shot_submit;
pub mod paper_algo_host;
pub mod paper_execution_host;
pub mod pipeline;
pub mod pipeline_metrics_report;
pub mod provider_registry;
pub mod reconcile_report;
pub mod risk_host;
pub mod spread_recovery_host;
pub mod venue_market_host;
pub mod worker_config;
pub mod worker_host;
pub mod worker_market_spec;
pub mod worker_metrics;
