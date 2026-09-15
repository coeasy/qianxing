//! Qianxing stable domain boundary.
//!
//! qx-core remains the single source of truth for deterministic trading
//! facts. qx-domain re-exports those canonical facts and owns only upper-layer
//! read models/metadata that do not mutate kernel state.

pub mod asset;
pub mod event;
pub mod ledger;
pub mod manifest;
pub mod order;
pub mod portfolio;
pub mod position;
pub mod trade;

pub use asset::{AssetClass, Instrument};
pub use event::{DomainEvent, EventId, EventKind, Priority};
pub use ledger::{Ledger, LedgerEntry, LedgerEntryKind, PositionState};
pub use manifest::RunManifest;
pub use order::{Fill, Order, OrderSide, OrderStatus, OrderTrace, Side};
pub use portfolio::Portfolio;
pub use position::Position;
pub use trade::Trade;
