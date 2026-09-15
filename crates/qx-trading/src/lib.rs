//! Qianxing trading orchestration layer V5.
//!
//! Handles order lifecycle orchestration. Matching and venue-specific
//! execution remain separated.

pub mod order_manager;
pub mod router;
pub mod execution;

pub use order_manager::OrderManager;
