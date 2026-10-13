use super::*;
use qx_guanxing::DataSourceId;

struct MockProvider {
    capability: ProviderCapability,
    fail: Option<ProviderErrorClass>,
}

impl DataProvider for MockProvider {
    fn capability(&self) -> &ProviderCapability {
        &self.capability
    }

    fn fetch(&self, _query: &DataQuery) -> Result<ProviderResult, ProviderError> {
        if let Some(class) = self.fail {
            return Err(ProviderError::new(class, "mock failure"));
        }
        let (record, payload) = row(&self.capability.provider_id, 1, 2, 10);
        let mut result = ProviderResult {
            schema_version: PROVIDER_RESULT_SCHEMA_VERSION,
            records: vec![record],
            payloads: vec![payload],
            provider_id: self.capability.provider_id.clone(),
            provider_version: self.capability.version.clone(),
            request_id: "req-1".into(),
            retry_chain: Vec::new(),
            received_at: 2,
            source_hash: 0,
        };
        result.source_hash = result.compute_source_hash();
        Ok(result)
    }
}

fn capability(id: &str, priority: u32, quality_score: u32) -> ProviderCapability {
    ProviderCapability {
        provider_id: id.into(),
        version: "v1".into(),
        data_kinds: [DataKind::Bar].into_iter().collect(),
        asset_classes: ["crypto".into()].into_iter().collect(),
        frequencies: ["1d".into()].into_iter().collect(),
        auth_scope: "public".into(),
        rate_limit_per_second: 10,
        freshness_seconds: 60,
        historical_start: 0,
        historical_end: 100,
        realtime: false,
        priority,
        quality_score,
        cost_score: 1,
    }
}

fn query() -> DataQuery {
    DataQuery {
        kind: DataKind::Bar,
        asset_class: "crypto".into(),
        instrument_set: BTreeSet::new(),
        field_set: BTreeSet::new(),
        frequency: "1d".into(),
        adjustment: "none".into(),
        quality_policy: "strict".into(),
        start: 1,
        end: 10,
        as_of: None,
    }
}

fn row(
    source: &str,
    event_time: u64,
    receive_time: u64,
    close: i128,
) -> (RawRecord, ProviderPayload) {
    let payload = ProviderPayload::new_json(
        "BTCUSDT.BINANCE",
        "application/vnd.qianxing.bar+json;v=1",
        serde_json::to_string(&qx_guanxing::Bar::new(
            event_time,
            close - 1,
            close + 2,
            close - 2,
            close,
            100,
        ))
        .unwrap(),
    );
    let record = RawRecord {
        source: DataSourceId::new(source),
        event_time,
        receive_time,
        payload_hash: payload.digest(),
        schema_version: 1,
    };
    (record, payload)
}

#[test]
fn selection_is_stable_and_explainable() {
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(MockProvider {
            capability: capability("backup", 2, 100),
            fail: None,
        }))
        .unwrap();
    registry
        .register(Box::new(MockProvider {
            capability: capability("primary", 1, 80),
            fail: None,
        }))
        .unwrap();
    assert_eq!(
        registry.candidates(&query()).unwrap(),
        ["primary", "backup"]
    );
}

#[test]
fn required_auth_scope_and_realtime_capability_filter_candidates() {
    let mut registry = ProviderRegistry::new();
    let mut public_realtime = capability("public-realtime", 1, 100);
    public_realtime.realtime = true;
    let mut private_historical = capability("private-history", 2, 100);
    private_historical.auth_scope = "account:read".into();
    let mut private_realtime = capability("private-realtime", 3, 100);
    private_realtime.auth_scope = "account:read".into();
    private_realtime.realtime = true;
    for selected in [public_realtime, private_historical, private_realtime] {
        registry
            .register(Box::new(MockProvider {
                capability: selected,
                fail: None,
            }))
            .unwrap();
    }

    let required = DataRequest::new(
        DataQuery {
            as_of: Some(10),
            ..query()
        },
        DataRequirements {
            required_auth_scope: Some("account:read".into()),
            require_realtime: true,
            max_staleness_ms: None,
        },
    );

    assert_eq!(
        registry.candidates_for_request(&required).unwrap(),
        ["private-realtime"]
    );
}

#[test]
fn python_flat_provider_request_filters_auth_realtime_and_freshness_metadata() {
    let payload = r#"{"adjustment":"none","as_of":6,"asset_class":"crypto","end":10,"field_set":["close"],"frequency":"1m","instrument_set":["BTCUSDT.BINANCE"],"kind":"Bar","max_staleness_ms":2,"quality_policy":"strict","require_realtime":true,"required_auth_scope":"account:read","start":1}"#;
    let request = DataRequest::from_json(payload).unwrap();
    assert_eq!(
        request.requirements.required_auth_scope.as_deref(),
        Some("account:read")
    );
    assert!(request.requirements.require_realtime);
    assert_eq!(request.requirements.max_staleness_ms, Some(2));

    let mut registry = ProviderRegistry::new();
    for (id, scope, realtime, priority) in [
        ("public", "public", false, 0),
        ("private-history", "account:read", false, 1),
        ("private-live", "account:read", true, 2),
    ] {
        let mut capability = capability(id, priority, 100);
        capability.frequencies = ["1m".into()].into_iter().collect();
        capability.auth_scope = scope.into();
        capability.realtime = realtime;
        registry
            .register(Box::new(MockProvider {
                capability,
                fail: None,
            }))
            .unwrap();
    }
    assert_eq!(
        registry.candidates_for_request(&request).unwrap(),
        ["private-live"]
    );
}

#[test]
fn realtime_provider_results_obey_requested_and_declared_freshness() {
    let mut source_capability = capability("realtime", 1, 100);
    source_capability.realtime = true;
    let mut realtime_request = DataRequest::new(
        DataQuery {
            as_of: Some(10),
            ..query()
        },
        DataRequirements {
            required_auth_scope: None,
            require_realtime: true,
            max_staleness_ms: Some(8),
        },
    );

    let result = MockProvider {
        capability: source_capability.clone(),
        fail: None,
    }
    .fetch(&realtime_request.query)
    .unwrap();
    assert!(result
        .validate_for_request(&realtime_request, &source_capability)
        .is_err());

    realtime_request.requirements.max_staleness_ms = Some(9);
    assert!(result
        .validate_for_request(&realtime_request, &source_capability)
        .is_ok());

    source_capability.freshness_seconds = 0;
    assert!(result
        .validate_for_request(&realtime_request, &source_capability)
        .is_err());
}

#[test]
fn registry_rejects_incomplete_capability_declarations() {
    let mut invalid = capability("invalid", 1, 100);
    invalid.version.clear();
    let mut registry = ProviderRegistry::new();
    assert!(matches!(
        registry.register(Box::new(MockProvider {
            capability: invalid,
            fail: None,
        })),
        Err(RegistryError::Provider(_))
    ));
}

#[test]
fn failover_keeps_retry_chain() {
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(MockProvider {
            capability: capability("primary", 1, 100),
            fail: Some(ProviderErrorClass::SwitchProvider),
        }))
        .unwrap();
    registry
        .register(Box::new(MockProvider {
            capability: capability("backup", 2, 90),
            fail: None,
        }))
        .unwrap();
    let result = registry.fetch_with_failover(&query()).unwrap();
    assert_eq!(result.provider_id, "backup");
    assert_eq!(result.retry_chain, ["primary", "backup"]);
}

#[test]
fn manual_intervention_stops_automatic_failover() {
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(MockProvider {
            capability: capability("primary", 1, 100),
            fail: Some(ProviderErrorClass::ManualIntervention),
        }))
        .unwrap();
    registry
        .register(Box::new(MockProvider {
            capability: capability("backup", 2, 90),
            fail: None,
        }))
        .unwrap();
    assert!(matches!(
        registry.fetch_with_failover(&query()),
        Err(RegistryError::Provider(ProviderError {
            class: ProviderErrorClass::ManualIntervention,
            ..
        }))
    ));
}

#[test]
fn in_memory_provider_applies_as_of_cutoff() {
    let records = vec![row("local", 1, 10, 10), row("local", 2, 20, 11)];
    let provider = InMemoryProvider::new(capability("local", 1, 100), records);
    let mut query = query();
    query.end = 2;
    query.as_of = Some(10);
    let result = provider.fetch(&query).unwrap();
    assert_eq!(result.records.len(), 1);
    assert_eq!(result.records.len(), 1);
    assert_eq!(result.payloads.len(), 1);
    assert_eq!(result.records[0].payload_hash, result.payloads[0].digest());
}

#[test]
fn provider_result_rejects_wrong_source_or_unsorted_records() {
    let (record_late, payload_late) = row("local", 2, 2, 11);
    let (record_early, payload_early) = row("local", 1, 1, 10);
    let mut result = ProviderResult {
        schema_version: PROVIDER_RESULT_SCHEMA_VERSION,
        records: vec![record_late, record_early],
        payloads: vec![payload_late, payload_early],
        provider_id: "local".into(),
        provider_version: "v1".into(),
        request_id: "r1".into(),
        retry_chain: vec!["local".into()],
        received_at: 2,
        source_hash: 0,
    };
    assert!(result.validate_for(&query()).is_err());
    result.records.swap(0, 1);
    result.records[0].source = DataSourceId::new("other");
    assert!(result.validate_for(&query()).is_err());
}

#[test]
fn source_hash_is_verified_before_accepting_provider_result() {
    let (record, payload) = row("local", 1, 2, 10);
    let mut result = ProviderResult {
        schema_version: PROVIDER_RESULT_SCHEMA_VERSION,
        records: vec![record],
        payloads: vec![payload],
        provider_id: "local".into(),
        provider_version: "v1".into(),
        request_id: "req".into(),
        retry_chain: Vec::new(),
        received_at: 2,
        source_hash: 0,
    };
    result.source_hash = result.compute_source_hash();
    assert!(result.validate_for(&query()).is_ok());
    result.payloads[0].json.push(' ');
    assert!(result.validate_for(&query()).is_err());
}

#[test]
fn provider_query_and_result_json_round_trip_preserve_provenance() {
    let query = query();
    let query_json = query.to_json().unwrap();
    assert_eq!(DataQuery::from_json(&query_json).unwrap(), query);
    let request = DataRequest::new(
        DataQuery {
            as_of: Some(10),
            ..query.clone()
        },
        DataRequirements {
            required_auth_scope: Some("account:read".into()),
            require_realtime: true,
            max_staleness_ms: Some(500),
        },
    );
    let request_json = request.to_json().unwrap();
    assert_eq!(DataRequest::from_json(&request_json).unwrap(), request);
    assert_eq!(
        DataRequest::from_json(&query_json).unwrap(),
        DataRequest::new(query.clone(), DataRequirements::default())
    );

    let provider = InMemoryProvider::new(capability("local", 1, 100), vec![row("local", 1, 2, 10)]);
    let result = provider.fetch(&query).unwrap();
    let result_json = result.to_json().unwrap();
    assert_eq!(ProviderResult::from_json(&result_json).unwrap(), result);
    let mut wire: serde_json::Value = serde_json::from_str(&result_json).unwrap();
    wire["source_hash"] = serde_json::Value::from(0_u64);
    assert!(ProviderResult::from_json(&wire.to_string()).is_err());
    let capability = capability("local", 1, 100);
    assert_eq!(
        ProviderCapability::from_json(&capability.to_json().unwrap()).unwrap(),
        capability
    );
}
