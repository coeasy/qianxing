//! Unified runtime contracts for Qianxing V2.
//!
//! This module defines the stable orchestration boundary between domain
//! events and execution modes. It intentionally does not own trading facts;
//! those remain in qx-core/qx-domain.

use serde::{Deserialize, Serialize};

/// Runtime execution mode shared by backtest, paper and live environments.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeMode {
    Backtest,
    Paper,
    Live,
}

/// Lifecycle phase of a running runtime instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimePhase {
    Created,
    Running,
    Draining,
    Stopped,
}

/// Stable runtime identity used for audit and replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeIdentity {
    pub run_id: String,
    pub mode: RuntimeMode,
    pub phase: RuntimePhase,
}

impl RuntimeIdentity {
    pub fn new(run_id: impl Into<String>, mode: RuntimeMode) -> Self {
        Self {
            run_id: run_id.into(),
            mode,
            phase: RuntimePhase::Created,
        }
    }

    pub fn start(mut self) -> Self {
        self.phase = RuntimePhase::Running;
        self
    }

    pub fn stop(mut self) -> Self {
        self.phase = RuntimePhase::Stopped;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_transition_is_deterministic() {
        let runtime = RuntimeIdentity::new("run-1", RuntimeMode::Backtest).start();
        assert_eq!(runtime.phase, RuntimePhase::Running);
        assert_eq!(runtime.stop().phase, RuntimePhase::Stopped);
    }
}
