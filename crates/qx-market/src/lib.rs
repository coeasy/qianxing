//! Qianxing V5 market data layer.
//!
//! qx-market converts external provider data into canonical domain events.
//! It must not contain strategy, execution or portfolio logic.

pub mod provider;
pub mod quality;

pub use provider::{MarketProvider, MarketTick};
