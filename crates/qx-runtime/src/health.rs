use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub ready: bool,
    pub component: String,
    pub message: String,
}

impl HealthSnapshot {
    pub fn ready(component: impl Into<String>) -> Self {
        Self {
            ready: true,
            component: component.into(),
            message: "ok".to_string(),
        }
    }
}
