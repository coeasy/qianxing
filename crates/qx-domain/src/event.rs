//! Stable domain access to the canonical causal event model.
//!
//! Event ordering, priorities and replay semantics remain owned by qx-core.

pub use qx_core::{Event as DomainEvent, EventKind, Priority};

/// Canonical event identity is the qx-core sequence number.
pub type EventId = u64;
