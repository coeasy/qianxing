//! Fetch historical funding events through the shared Rust ProviderRegistry, then execute the
//! ordinary shared multi-leg backtest with those exact events.

use super::attach;
use super::guard::guard_panics;
use super::run_multi_leg_backtest::run_inner;
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiLegBacktestOutcome, MultiLegBacktestSpec,
    RunContext,
};
use qx_core::{FundingRateEvent, InstrumentId};
use qx_provider::{DataKind, DataRequest, ProviderErrorClass, ProviderRegistry, RegistryError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const FUNDING_RATE_JSON_CONTENT_TYPE: &str = "application/vnd.qianxing.funding-rate+json;v=1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FundingRateProviderProvenance {
    pub provider_id: String,
    pub provider_version: String,
    pub request_id: String,
    pub source_hash: String,
    pub query_hash: String,
    pub retry_chain: Vec<String>,
    pub record_count: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FundingRatePayload {
    rate_bps: i64,
}

/// Fetch, validate and execute provider-supplied historical funding through the shared Rust core.
pub(super) fn run_multi_leg_backtest_with_funding_provider(
    registry: &ProviderRegistry,
    request: &DataRequest,
    spec: &MultiLegBacktestSpec,
    context: &RunContext,
) -> Result<MultiLegBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "Provider 历史资金费率多腿回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        spec.validate()?;
        validate_request(request, spec)?;
        if spec.funding_bps != 0 || !spec.funding_events.is_empty() {
            return Err(invalid(
                "Provider funding workflow cannot mix provider history with inline funding inputs",
            ));
        }
        let fetched = registry.fetch_request(request).map_err(provider_error)?;
        if fetched.records.is_empty() {
            return Err(AppError::new(
                AppErrorCategory::DataUnavailable,
                "funding Provider returned no historical settlement events",
            ));
        }
        let funding_events = fetched
            .records
            .iter()
            .zip(&fetched.payloads)
            .map(|(record, payload)| {
                if payload.content_type != FUNDING_RATE_JSON_CONTENT_TYPE {
                    return Err(invalid(format!(
                        "funding Provider payload content_type is unsupported: {}",
                        payload.content_type
                    )));
                }
                let instrument = InstrumentId::parse(&payload.instrument)
                    .filter(|instrument| instrument.to_string() == payload.instrument)
                    .ok_or_else(|| {
                        invalid(format!(
                            "funding Provider instrument is not canonical: {}",
                            payload.instrument
                        ))
                    })?;
                let rate: FundingRatePayload =
                    serde_json::from_str(&payload.json).map_err(|error| {
                        invalid(format!("funding Provider rate payload is invalid: {error}"))
                    })?;
                Ok(FundingRateEvent {
                    instrument,
                    settlement_ts: record.event_time,
                    rate_bps: rate.rate_bps,
                })
            })
            .collect::<Result<Vec<_>, AppError>>()?;
        let provenance = FundingRateProviderProvenance {
            provider_id: fetched.provider_id,
            provider_version: fetched.provider_version,
            request_id: fetched.request_id,
            source_hash: format!("{:016x}", fetched.source_hash),
            query_hash: format!("{:016x}", request.query.digest()),
            retry_chain: fetched.retry_chain,
            record_count: funding_events.len(),
        };
        let mut resolved_spec = spec.clone();
        resolved_spec.funding_events = funding_events;
        resolved_spec.validate()?;
        run_inner(&resolved_spec, context, None, None, Some(&provenance))?
            .ok_or_else(|| invalid("Provider funding backtest did not produce a report"))
    })
    .map_err(|error| attach(error, &correlation_id))
}

impl MultiLegBacktestSpec {
    /// Run with historical funding fetched and lineage-checked by the shared Rust ProviderRegistry.
    pub fn run_with_funding_provider(
        &self,
        registry: &ProviderRegistry,
        request: &DataRequest,
        context: &RunContext,
    ) -> Result<MultiLegBacktestOutcome, AppError> {
        run_multi_leg_backtest_with_funding_provider(registry, request, self, context)
    }
}

fn validate_request(request: &DataRequest, spec: &MultiLegBacktestSpec) -> Result<(), AppError> {
    request
        .query
        .validate()
        .and_then(|()| request.requirements.validate_for(&request.query))
        .map_err(|error| invalid(error.message))?;
    let legs = [
        spec.primary_market_spec
            .as_ref()
            .map(|market| &market.instrument),
        spec.reference_market_spec
            .as_ref()
            .map(|market| &market.instrument),
    ];
    let allowed = legs.into_iter().flatten().collect::<BTreeSet<_>>();
    if request.query.kind != DataKind::Financial
        || request.query.field_set != BTreeSet::from(["funding_rate_bps".to_string()])
        || request.query.frequency != "funding_8h"
        || request.requirements.require_realtime
        || request.query.instrument_set.is_empty()
        || request.query.instrument_set.len() > 2
        || request.query.instrument_set.iter().any(|instrument| {
            InstrumentId::parse(instrument).is_none_or(|parsed| {
                parsed.to_string() != *instrument || !allowed.contains(&parsed)
            })
        })
    {
        return Err(invalid(
            "funding Provider request requires Financial/funding_8h/funding_rate_bps and one or two configured multi-leg instruments",
        ));
    }
    Ok(())
}

fn provider_error(error: RegistryError) -> AppError {
    match error {
        RegistryError::NoProvider => AppError::new(
            AppErrorCategory::DataUnavailable,
            "no Provider satisfies the historical funding request",
        ),
        RegistryError::Duplicate(id) => AppError::new(
            AppErrorCategory::InvalidInput,
            format!("duplicate Provider registration: {id}"),
        ),
        RegistryError::Provider(error) => {
            let category = match error.class {
                ProviderErrorClass::SwitchProvider => AppErrorCategory::DataUnavailable,
                ProviderErrorClass::ManualIntervention => AppErrorCategory::PermissionDenied,
                ProviderErrorClass::Permanent => AppErrorCategory::InvalidInput,
            };
            AppError::new(
                category,
                format!("funding Provider fetch failed: {}", error.message),
            )
        }
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuiltinStrategySpec, MultiLegBacktestSpec};
    use qx_core::{TradingInstrumentSpec, TradingProduct, SCALE};
    use qx_provider::{
        DataProvider, DataQuery, DataRequirements, ProviderCapability, ProviderError,
        ProviderPayload, ProviderResult,
    };
    use std::collections::BTreeSet;

    struct HistoricalFundingProvider {
        capability: ProviderCapability,
        rows: Vec<(u64, u64, ProviderPayload)>,
    }

    impl DataProvider for HistoricalFundingProvider {
        fn capability(&self) -> &ProviderCapability {
            &self.capability
        }

        fn fetch(&self, query: &qx_provider::DataQuery) -> Result<ProviderResult, ProviderError> {
            ProviderResult::from_payloads(
                &self.capability.provider_id,
                &self.capability.version,
                format!("funding-{:016x}", query.digest()),
                query.end,
                self.rows.clone(),
            )
        }
    }

    fn market(instrument: &str) -> TradingInstrumentSpec {
        let symbol = instrument.split_once('.').unwrap().0;
        let (base, quote) = symbol.split_at(3);
        TradingInstrumentSpec {
            instrument: InstrumentId::parse(instrument).unwrap(),
            product: TradingProduct::Perpetual,
            base_currency: base.into(),
            quote_currency: quote.into(),
            settlement_currency: "USDT".into(),
            contract_size: SCALE,
            linear: true,
            inverse: false,
            price_tick: 1_000,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 20,
            maintenance_margin_bps: 500,
            valid_from: 0,
            valid_to: None,
        }
    }

    fn registry() -> ProviderRegistry {
        let instrument = "BTCUSDT.BINANCE";
        let capability = ProviderCapability {
            provider_id: "fixture-funding".into(),
            version: "v1".into(),
            data_kinds: BTreeSet::from([DataKind::Financial]),
            asset_classes: BTreeSet::from(["crypto".into()]),
            frequencies: BTreeSet::from(["funding_8h".into()]),
            auth_scope: "public".into(),
            rate_limit_per_second: 100,
            freshness_seconds: 0,
            historical_start: 0,
            historical_end: u64::MAX,
            realtime: false,
            priority: 0,
            quality_score: 100,
            cost_score: 0,
        };
        let rows = [
            (144_000_000, 172_800_000, 100),
            (172_800_000, 172_800_000, -50),
        ]
        .into_iter()
        .map(|(ts, received, rate)| {
            (
                ts,
                received,
                ProviderPayload::new_json(
                    instrument,
                    FUNDING_RATE_JSON_CONTENT_TYPE,
                    format!(r#"{{"rate_bps":{rate}}}"#),
                ),
            )
        })
        .collect();
        let mut registry = ProviderRegistry::new();
        registry
            .register(Box::new(HistoricalFundingProvider { capability, rows }))
            .unwrap();
        registry
    }

    fn request() -> DataRequest {
        DataRequest::new(
            DataQuery {
                kind: DataKind::Financial,
                asset_class: "crypto".into(),
                instrument_set: BTreeSet::from(["BTCUSDT.BINANCE".into()]),
                field_set: BTreeSet::from(["funding_rate_bps".into()]),
                frequency: "funding_8h".into(),
                adjustment: "none".into(),
                quality_policy: "strict".into(),
                start: 144_000_000,
                end: 172_800_000,
                as_of: None,
            },
            DataRequirements::default(),
        )
    }

    fn backtest(root: &std::path::Path) -> MultiLegBacktestSpec {
        let fixture_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("deploy");
        let mut strategy = BuiltinStrategySpec::new(
            qx_strategy::BuiltinStrategyKind::PairsArbitrage,
            "provider-pairs",
        );
        strategy.quantity_raw = SCALE;
        strategy.fast_window = 5;
        strategy.slow_window = 20;
        strategy.period = 14;
        strategy.threshold_bps = 100;
        let mut spec = MultiLegBacktestSpec::new(
            "provider-funding-e2e",
            fixture_dir
                .join("qianxing.bar-frame.pairs-primary.example.json")
                .to_string_lossy(),
            fixture_dir
                .join("qianxing.bar-frame.pairs-reference.example.json")
                .to_string_lossy(),
            "USDT",
            1_000_000_000_000_000,
            1_000_000_000_000_000,
            7,
            root.join("out").to_string_lossy(),
            strategy,
        );
        spec.primary_market_spec = Some(market("BTCUSDT.BINANCE"));
        spec.reference_market_spec = Some(market("ETHUSDT.BINANCE"));
        spec
    }

    fn context() -> RunContext {
        RunContext::new(CallerCapability::Research, "provider-funding-test")
    }

    #[test]
    fn provider_historical_rates_flow_through_the_shared_multileg_core_with_lineage() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join(format!("provider-funding-{}", std::process::id()));
        let output = run_multi_leg_backtest_with_funding_provider(
            &registry(),
            &request(),
            &backtest(&root),
            &context(),
        )
        .expect("funding history runs in the shared Rust app workflow");
        let artifact: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&output.attribution_artifact).expect("funding report exists"),
        )
        .unwrap();
        assert_eq!(
            artifact["funding_rate_input"],
            "caller_supplied_historical_events"
        );
        assert_eq!(artifact["funding_rate_events"].as_array().unwrap().len(), 2);
        assert_eq!(
            artifact["funding_provider_provenance"]["provider_id"],
            "fixture-funding"
        );
        assert_eq!(artifact["funding_provider_provenance"]["record_count"], 2);
        assert_eq!(
            artifact["funding_provider_provenance"]["query_hash"],
            format!("{:016x}", request().query.digest())
        );
        assert_eq!(
            artifact["execution_funding_events"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn provider_funding_rejects_wrong_field_or_unconfigured_instrument_before_fetch() {
        let mut request = request();
        request.query.field_set = BTreeSet::from(["close".into()]);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target");
        let error = run_multi_leg_backtest_with_funding_provider(
            &ProviderRegistry::new(),
            &request,
            &backtest(&root),
            &context(),
        )
        .unwrap_err();
        assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    }
}
