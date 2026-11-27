use qx_core::{event_source_kind, EventContext, EventMetadata, EVENT_METADATA_SCHEMA_VERSION};
use serde_json::Value;

const V1_FIXTURE: &str = include_str!("fixtures/event-metadata-v1.json");
const V2_FIXTURE: &str = include_str!("fixtures/event-metadata-v2.json");
const V1_SCHEMA: &str = include_str!("../../../schemas/event-metadata-v1.json");
const V2_SCHEMA: &str = include_str!("../../../schemas/event-metadata-v2.json");

#[test]
fn event_metadata_v2_matches_the_registered_cross_process_golden() {
    assert_eq!(EVENT_METADATA_SCHEMA_VERSION, 2);

    let metadata = EventMetadata {
        source_id: "okx-market".into(),
        source_kind: event_source_kind::MARKET_QUOTE.into(),
        dedup_key: "okx:BTC-USDT:quote:1730000000000".into(),
        rule_version: "market-v2".into(),
        context: EventContext {
            tenant_id: "tenant-demo".into(),
            run_id: "run-42".into(),
            account_id: "account-main".into(),
            portfolio_id: "portfolio-spot".into(),
            strategy_id: "momentum".into(),
            signal_id: "signal-17".into(),
            intent_id: String::new(),
        },
        ..EventMetadata::default()
    };

    let schema: Value = serde_json::from_str(V2_SCHEMA).expect("v2 schema is valid JSON");
    let fixture: Value = serde_json::from_str(V2_FIXTURE).expect("v2 golden is valid JSON");
    let encoded = serde_json::to_value(&metadata).expect("metadata serializes");
    assert_eq!(schema["properties"]["schema_version"]["const"], 2);
    assert!(schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "source_kind"));
    assert_eq!(encoded, fixture);

    let restored: EventMetadata = serde_json::from_value(fixture).expect("v2 golden deserializes");
    assert_eq!(restored, metadata);
    restored.validate().expect("v2 metadata is valid");
}

#[test]
fn historical_v1_metadata_replays_with_the_documented_source_default() {
    let schema: Value = serde_json::from_str(V1_SCHEMA).expect("v1 schema is valid JSON");
    let fixture: Value = serde_json::from_str(V1_FIXTURE).expect("v1 golden is valid JSON");
    assert_eq!(schema["properties"]["schema_version"]["const"], 1);
    assert!(fixture.get("source_kind").is_none());

    let restored: EventMetadata = serde_json::from_value(fixture).expect("v1 golden replays");
    assert_eq!(restored.schema_version, 1);
    assert_eq!(restored.source_kind, event_source_kind::INTERNAL);
    assert_eq!(restored.source_id, "okx-market");
    assert_eq!(restored.rule_version, "market-v1");
    restored
        .validate()
        .expect("replayed v1 metadata remains valid");
}
