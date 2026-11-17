use qx_strategy::{
    MarketEvent, Strategy, StrategyContext, StrategyDecision, StrategyStopContextV1,
    StrategyStopReason,
};
use serde_json::Value;

struct StopRecorder {
    callbacks: Vec<String>,
}

impl Strategy for StopRecorder {
    fn on_event(
        &mut self,
        _context: &StrategyContext,
        _event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        Err("stop golden does not dispatch market events".into())
    }

    fn on_stop(&mut self) -> Result<(), String> {
        self.callbacks.push("on_stop:host_stop".into());
        Ok(())
    }
}

#[test]
fn stop_context_matches_the_shared_wire_fixture() {
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/strategy-stop-context-v1.json"))
            .expect("shared stop-context fixture is valid JSON");
    let stop: StrategyStopContextV1 = serde_json::from_value(expected.clone())
        .expect("Rust StrategyStopContextV1 accepts the shared fixture");

    stop.validate()
        .expect("stop context meets the common contract");
    assert_eq!(stop.reason, StrategyStopReason::HostStop);
    assert_eq!(stop.context.as_ref().unwrap().as_of, 100);
    assert_eq!(serde_json::to_value(stop).unwrap(), expected);

    let matrix: Value =
        serde_json::from_str(include_str!("fixtures/strategy-lifecycle-matrix-v1.json")).unwrap();
    let mut recorder = StopRecorder {
        callbacks: Vec::new(),
    };
    recorder
        .on_stop_with_context(&serde_json::from_value(expected).unwrap())
        .unwrap();
    assert_eq!(
        recorder.callbacks,
        matrix["expected_stop_callbacks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    );
}

#[test]
fn stop_context_rejects_unknown_schema_and_invalid_final_context() {
    let mut stop = StrategyStopContextV1::host_stop(None);
    stop.schema_version = 2;
    assert!(stop.validate().unwrap_err().contains("schema_version"));

    let value: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/strategy-stop-context-v1.json")).unwrap();
    let mut stop: StrategyStopContextV1 = serde_json::from_value(value).unwrap();
    stop.context.as_mut().unwrap().as_of = 0;
    assert!(stop.validate().unwrap_err().contains("StrategyContext"));
}
