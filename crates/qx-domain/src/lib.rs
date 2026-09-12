//! Qianxing domain contracts.
//!
//! This crate defines stable business facts and intentionally does not depend
//! on providers, strategies, execution venues, or storage implementations.

pub mod asset;
pub mod event;
pub mod order;
pub mod portfolio;
pub mod position;
pub mod manifest;

pub use asset::{AssetClass, Instrument};
pub use event::{DomainEvent, EventId};
pub use order::{Order, OrderSide, OrderStatus};
pub use portfolio::Portfolio;
pub use position::Position;
pub use manifest::RunManifest;
