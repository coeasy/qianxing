//! Qianxing domain contracts.
//!
//! This crate defines stable business facts and intentionally does not depend
//! on providers, strategies, execution venues, or storage implementations.

pub mod asset;
pub mod event;
pub mod manifest;
pub mod order;
pub mod portfolio;
pub mod position;

pub use asset::{AssetClass, Instrument};
pub use event::{DomainEvent, EventId};
pub use manifest::RunManifest;
pub use order::{Order, OrderSide, OrderStatus};
pub use portfolio::Portfolio;
pub use position::Position;
