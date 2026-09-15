//! Qianxing trading execution layer V5.
//!
//! Handles order lifecycle orchestration. Matching and venue-specific
//! execution remain separated.

pub mod order_manager;
pub mod router;

pub use order_manager::OrderManager;
