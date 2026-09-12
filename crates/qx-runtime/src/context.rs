use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Shared runtime context for backtest, paper and live modes.
///
/// Runtime owns orchestration only. Trading facts remain in qx-domain/qx-core.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeContext {
    pub run_id: String,
    pub mode: RuntimeMode,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum RuntimeMode {
    #[default]
    Backtest,
    Paper,
    Live,
}

impl RuntimeContext {
    pub fn new(run_id: impl Into<String>, mode: RuntimeMode) -> Self {
        Self {
            run_id: run_id.into(),
            mode,
            metadata: BTreeMap::new(),
        }
    }
}
