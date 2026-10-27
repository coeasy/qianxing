//! Python strategy lifecycle callbacks adapted to the shared Rust contract.

use super::*;
use qx_strategy::{StrategyFillUpdateV2, StrategyStopContextV1};

impl Strategy for PythonBacktestStrategy {
    fn on_init(&mut self, context: &StrategyContext) -> Result<(), String> {
        let context_json = serde_json::to_string(context).map_err(|error| error.to_string())?;
        Python::attach(|py| {
            self.callback
                .bind(py)
                .call_method1("_qx_on_init", (context_json,))
                .map(|_| ())
                .map_err(|error| format!("Python strategy init callback failed: {error}"))
        })
    }

    fn on_start(&mut self, context: &StrategyContext) -> Result<(), String> {
        let context_json = serde_json::to_string(context).map_err(|error| error.to_string())?;
        Python::attach(|py| {
            self.callback
                .bind(py)
                .call_method1("_qx_on_start", (context_json,))
                .map(|_| ())
                .map_err(|error| format!("Python strategy start callback failed: {error}"))
        })
    }

    fn on_event(
        &mut self,
        context: &StrategyContext,
        event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        let context_json = serde_json::to_string(context).map_err(|error| error.to_string())?;
        let event_json = serde_json::to_string(event).map_err(|error| error.to_string())?;
        Python::attach(|py| {
            let response = self
                .callback
                .bind(py)
                .call_method1("_qx_on_event", (context_json, event_json))
                .and_then(|value| value.extract::<String>())
                .map_err(|error| format!("Python strategy callback failed: {error}"))?;
            serde_json::from_str(&response).map_err(|error| {
                format!("Python strategy returned invalid StrategyDecision JSON: {error}")
            })
        })
    }

    fn on_order_update(
        &mut self,
        context: &StrategyContext,
        update: &str,
    ) -> Result<Option<StrategyDecision>, String> {
        let context_json = serde_json::to_string(context).map_err(|error| error.to_string())?;
        Python::attach(|py| {
            let callback = self.callback.bind(py);
            if !callback
                .hasattr("_qx_on_order_update")
                .map_err(|error| format!("Python strategy callback inspection failed: {error}"))?
            {
                return Ok(None);
            }
            let response = callback
                .call_method1("_qx_on_order_update", (context_json, update))
                .and_then(|value| value.extract::<String>())
                .map_err(|error| {
                    format!("Python strategy order update callback failed: {error}")
                })?;
            serde_json::from_str::<Option<StrategyDecision>>(&response).map_err(|error| {
                format!("Python strategy returned invalid order update decision JSON: {error}")
            })
        })
    }

    fn on_strategy_fill_update_v2(
        &mut self,
        context: &StrategyContext,
        update: &StrategyFillUpdateV2,
    ) -> Result<Option<StrategyDecision>, String> {
        update.validate_for(context)?;
        let context_json = serde_json::to_string(context).map_err(|error| error.to_string())?;
        let update_json = serde_json::to_string(update).map_err(|error| error.to_string())?;
        Python::attach(|py| {
            let callback = self.callback.bind(py);
            if !callback
                .hasattr("_qx_on_fill_update_v2")
                .map_err(|error| format!("Python strategy callback inspection failed: {error}"))?
            {
                let legacy = serde_json::to_string(&update.legacy_projection())
                    .map_err(|error| error.to_string())?;
                if !callback.hasattr("_qx_on_order_update").map_err(|error| {
                    format!("Python strategy callback inspection failed: {error}")
                })? {
                    return Ok(None);
                }
                let response = callback
                    .call_method1("_qx_on_order_update", (context_json, legacy))
                    .and_then(|value| value.extract::<String>())
                    .map_err(|error| {
                        format!("Python strategy order update callback failed: {error}")
                    })?;
                return serde_json::from_str::<Option<StrategyDecision>>(&response).map_err(
                    |error| {
                        format!(
                            "Python strategy returned invalid order update decision JSON: {error}"
                        )
                    },
                );
            }
            let response = callback
                .call_method1("_qx_on_fill_update_v2", (context_json, update_json))
                .and_then(|value| value.extract::<String>())
                .map_err(|error| format!("Python strategy fill update callback failed: {error}"))?;
            serde_json::from_str::<Option<StrategyDecision>>(&response).map_err(|error| {
                format!("Python strategy returned invalid fill update decision JSON: {error}")
            })
        })
    }

    fn on_strategy_order_status(
        &mut self,
        context: &StrategyContext,
        update: &qx_strategy::StrategyOrderStatusUpdate,
    ) -> Result<Option<StrategyDecision>, String> {
        update.validate_for(context)?;
        let context_json = serde_json::to_string(context).map_err(|error| error.to_string())?;
        let update_json = serde_json::to_string(update).map_err(|error| error.to_string())?;
        Python::attach(|py| {
            let callback = self.callback.bind(py);
            if !callback
                .hasattr("_qx_on_order_status_update")
                .map_err(|error| format!("Python strategy callback inspection failed: {error}"))?
            {
                return Ok(None);
            }
            let response = callback
                .call_method1("_qx_on_order_status_update", (context_json, update_json))
                .and_then(|value| value.extract::<String>())
                .map_err(|error| {
                    format!("Python strategy order status callback failed: {error}")
                })?;
            serde_json::from_str::<Option<StrategyDecision>>(&response).map_err(|error| {
                format!("Python strategy returned invalid order status decision JSON: {error}")
            })
        })
    }

    fn on_stop(&mut self) -> Result<(), String> {
        Python::attach(|py| {
            self.callback
                .bind(py)
                .call_method0("_qx_on_stop")
                .map(|_| ())
                .map_err(|error| format!("Python strategy stop callback failed: {error}"))
        })
    }

    fn on_stop_with_context(&mut self, stop: &StrategyStopContextV1) -> Result<(), String> {
        stop.validate()?;
        let stop_json = serde_json::to_string(stop).map_err(|error| error.to_string())?;
        Python::attach(|py| {
            let callback = self.callback.bind(py);
            if callback
                .hasattr("_qx_on_stop_with_context")
                .map_err(|error| format!("Python strategy callback inspection failed: {error}"))?
            {
                callback
                    .call_method1("_qx_on_stop_with_context", (stop_json,))
                    .map(|_| ())
                    .map_err(|error| format!("Python strategy stop callback failed: {error}"))
            } else {
                callback
                    .call_method0("_qx_on_stop")
                    .map(|_| ())
                    .map_err(|error| format!("Python strategy stop callback failed: {error}"))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_strategy::{StrategyStopContextV1, StrategyStopReason};

    #[test]
    fn forwards_versioned_terminal_reason_to_python_callback() {
        Python::initialize();
        Python::attach(|py| {
            let module = PyModule::from_code(
                py,
                c"class Callback:\n    def __init__(self): self.stop = None\n    def _qx_on_stop_with_context(self, payload): self.stop = payload\n",
                c"strategy_stop_test.py",
                c"strategy_stop_test",
            )
            .expect("test callback module should load");
            let callback = module
                .getattr("Callback")
                .and_then(|class| class.call0())
                .expect("test callback should initialize")
                .unbind();
            let mut strategy = PythonBacktestStrategy { callback };
            strategy
                .on_stop_with_context(&StrategyStopContextV1::new(
                    StrategyStopReason::Cancelled,
                    None,
                ))
                .expect("Python callback should accept stop context");
            let payload = strategy
                .callback
                .bind(py)
                .getattr("stop")
                .and_then(|value| value.extract::<String>())
                .expect("stop callback should retain its payload");
            let decoded: serde_json::Value =
                serde_json::from_str(&payload).expect("payload should be JSON");
            assert_eq!(decoded["schema_version"], 1);
            assert_eq!(decoded["reason"], "cancelled");
            assert!(decoded["context"].is_null());
        });
    }
}
