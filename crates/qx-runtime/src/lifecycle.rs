use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum LifecycleState {
    Created,
    Running,
    Draining,
    Stopped,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeLifecycle {
    state: LifecycleState,
}

impl Default for RuntimeLifecycle {
    fn default() -> Self {
        Self { state: LifecycleState::Created }
    }
}

impl RuntimeLifecycle {
    pub fn state(&self) -> LifecycleState {
        self.state
    }

    pub fn start(&mut self) {
        self.state = LifecycleState::Running;
    }

    pub fn stop(&mut self) {
        self.state = LifecycleState::Stopped;
    }
}
