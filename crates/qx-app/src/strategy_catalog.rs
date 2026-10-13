//! The canonical discoverable built-in strategy catalog shared by all SDKs.

use qx_strategy::BuiltinStrategyKind;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltinStrategyInfo {
    pub name: String,
    pub description: String,
    pub signal_knobs: Vec<String>,
    pub requires_reference_leg: bool,
}

pub fn builtin_strategy_catalog() -> Vec<BuiltinStrategyInfo> {
    BuiltinStrategyKind::ALL
        .into_iter()
        .map(|kind| BuiltinStrategyInfo {
            name: kind.name().to_string(),
            description: kind.description().to_string(),
            signal_knobs: kind
                .signal_knobs()
                .iter()
                .map(|knob| knob.field().to_string())
                .collect(),
            requires_reference_leg: kind.needs_reference_leg(),
        })
        .collect()
}
