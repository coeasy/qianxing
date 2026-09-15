//! Qianxing kernel V5 foundation.
//!
//! Kernel owns deterministic primitives only:
//! - event identity
//! - clock abstraction
//! - replay boundaries
//! - snapshots

pub mod clock;
pub mod event;

pub use clock::{Clock, SystemClock};
pub use event::{EventEnvelope, EventId};
