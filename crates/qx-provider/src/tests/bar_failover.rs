use super::*;

struct FixedResultProvider {
    capability: ProviderCapability,
    result: ProviderResult,
}

impl DataProvider for FixedResultProvider {
    fn capability(&self) -> &ProviderCapability {
        &self.capability
    }

    fn fetch(&self, _query: &DataQuery) -> Result<ProviderResult, ProviderError> {
        Ok(self.result.clone())
    }
}

#[test]
fn bar_normalization_failure_fails_over_and_binds_the_full_retry_chain() {
    let mut registry = ProviderRegistry::new();
    let invalid_bar = ProviderResult::from_payloads(
        "primary",
        "v1",
        "primary-request",
        2,
        vec![(
            2,
            2,
            ProviderPayload::new_json(
                "BTCUSDT.BINANCE",
                "application/vnd.qianxing.bar+json;v=1",
                "{}",
            ),
        )],
    )
    .unwrap();
    registry
        .register(Box::new(FixedResultProvider {
            capability: capability("primary", 1, 100),
            result: invalid_bar,
        }))
        .unwrap();

    let (backup_record, backup_payload) = row("backup", 2, 2, 100);
    let backup_result = ProviderResult::from_payloads(
        "backup",
        "v1",
        "backup-request",
        2,
        vec![(
            backup_record.event_time,
            backup_record.receive_time,
            backup_payload,
        )],
    )
    .unwrap();
    registry
        .register(Box::new(FixedResultProvider {
            capability: capability("backup", 2, 90),
            result: backup_result,
        }))
        .unwrap();

    let request = DataRequest::new(
        DataQuery {
            instrument_set: ["BTCUSDT.BINANCE".into()].into_iter().collect(),
            ..query()
        },
        DataRequirements::default(),
    );
    let fetched = registry
        .fetch_bar_frame(&request)
        .expect("malformed primary Bar payload should fall back to the valid source");

    assert_eq!(fetched.result.provider_id, "backup");
    assert_eq!(fetched.result.retry_chain, ["primary", "backup"]);
    assert_eq!(fetched.frame.close_raw, [100]);
    assert!(fetched
        .frame
        .source
        .0
        .contains(&format!("source={:016x}", fetched.result.source_hash)));
}
