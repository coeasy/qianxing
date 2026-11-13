use qx_strategy::{
    MarketEvent, Strategy, StrategyContext, StrategyDecision, StrategyErrorStage, StrategyErrorV1,
};
use serde_json::Value;

struct ErrorRecorder {
    callbacks: Vec<String>,
}

impl Strategy for ErrorRecorder {
    fn on_event(
        &mut self,
        _context: &StrategyContext,
        _event: &MarketEvent,
    ) -> Result<StrategyDecision, String> {
        Err("error golden does not dispatch market events".into())
    }

    fn on_error(&mut self, error: &StrategyErrorV1) -> Result<(), String> {
        let value = serde_json::to_value(error).map_err(|caught| caught.to_string())?;
        let stage = value["stage"]
            .as_str()
            .ok_or_else(|| "StrategyErrorV1 stage did not serialize as text".to_string())?;
        self.callbacks.push(format!("on_error_v1:{stage}"));
        Ok(())
    }
}

#[test]
fn strategy_error_v1_matches_shared_golden_fixture() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/strategy-error-v1.json"))
        .expect("shared strategy error fixture is valid JSON");
    let error: StrategyErrorV1 =
        serde_json::from_value(fixture.clone()).expect("decode StrategyErrorV1 fixture");
    error.validate().expect("fixture satisfies Rust contract");
    assert_eq!(error.stage, StrategyErrorStage::MarketEvent);
    assert_eq!(serde_json::to_value(error).unwrap(), fixture);

    let error: StrategyErrorV1 = serde_json::from_value(fixture).unwrap();
    let matrix: Value =
        serde_json::from_str(include_str!("fixtures/strategy-lifecycle-matrix-v1.json")).unwrap();
    let mut recorder = ErrorRecorder {
        callbacks: Vec::new(),
    };
    recorder.on_error(&error).unwrap();
    assert_eq!(
        recorder.callbacks,
        matrix["expected_error_callbacks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    );
}
