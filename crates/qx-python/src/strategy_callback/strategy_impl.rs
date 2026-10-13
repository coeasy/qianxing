//! Python strategy lifecycle callbacks adapted to the shared Rust contract.

use super::*;

impl Strategy for PythonBacktestStrategy {
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
}
