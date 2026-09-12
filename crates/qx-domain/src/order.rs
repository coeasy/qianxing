//! Stable domain access to the canonical qx-core order model.
//!
//! qx-domain must not maintain a second order state machine. Execution facts
//! remain owned by qx-core and are re-exported here for upper-layer contracts.

pub use qx_core::{Fill, Order, OrderStatus, OrderTrace, Side};
pub use qx_core::Side as OrderSide;
