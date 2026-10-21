//! Resolved execution-cost binding shared by backtests and Paper execution.
//!
//! Facades own configuration-file lookup and path diagnostics. Once a rules object has been
//! loaded, this type keeps the fee/latency models and their provenance together.

use qx_core::FeeModel;
use qx_xingban::{ExecutionCostRules, LatencyModel};

#[derive(Debug)]
pub struct ExecutionCostBinding {
    pub rules: ExecutionCostRules,
    /// The resolved rules file, if a facade loaded one.
    pub loaded_from: Option<String>,
    /// Distinguishes a runtime-config default from the built-in default.
    pub from_runtime_config: bool,
}

impl ExecutionCostBinding {
    pub fn builtin_default() -> Self {
        Self::with_default_rules(false)
    }

    pub fn runtime_config_default() -> Self {
        Self::with_default_rules(true)
    }

    pub fn from_rules_file(rules: ExecutionCostRules, path: impl Into<String>) -> Self {
        Self {
            rules,
            loaded_from: Some(path.into()),
            from_runtime_config: true,
        }
    }

    pub fn fee_model(&self) -> Box<dyn FeeModel + Send> {
        self.rules.fee_model()
    }

    pub fn latency_model(&self) -> Box<dyn LatencyModel + Send> {
        self.rules.latency_model()
    }

    /// Provenance for reports; rate values themselves are carried in model descriptors.
    pub fn source(&self) -> String {
        match &self.loaded_from {
            Some(path) => format!("cost-rules-file:{path}"),
            None if self.from_runtime_config => "runtime-config-default".to_string(),
            None => "builtin-default".to_string(),
        }
    }

    /// Provenance for a Bar run whose A-share snapshot supplies fees while a cost file may still
    /// supply latency. Keep both sources visible because the effective models have different owners.
    pub fn source_with_ashare_rules(&self, ashare_rules_path: Option<&str>) -> String {
        match (ashare_rules_path, self.loaded_from.as_deref()) {
            (Some(rules_path), Some(cost_path)) => {
                format!("ashare-rules:{rules_path}+cost-rules-file:{cost_path}")
            }
            (Some(rules_path), None) => format!("ashare-rules:{rules_path}"),
            (None, _) => self.source(),
        }
    }

    fn with_default_rules(from_runtime_config: bool) -> Self {
        Self {
            rules: ExecutionCostRules::default(),
            loaded_from: None,
            from_runtime_config,
        }
    }
}
