//! Provider-backed Tick/OrderBook workflow. Provider provenance is embedded in the
//! immutable DepthFrame source identity before the shared Rust depth engine runs.

use super::{
    attach, guard::guard_panics, run_depth_backtest, run_provider_backtest_with_manifest::freeze,
};
use crate::{
    AppError, AppErrorCategory, CallerCapability, DepthBacktestOutcome, DepthBacktestSpec,
    RunContext,
};
use qx_provider::{DataKind, DataRequest, ProviderErrorClass, ProviderRegistry, RegistryError};
use qx_xingban::{DepthFrame, OrderBookSnapshot};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const ORDER_BOOK_SNAPSHOT_CONTENT_TYPE: &str =
    "application/vnd.qianxing.order-book-snapshot+json;v=1";
pub const PROVIDER_DEPTH_BACKTEST_SPEC_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDepthBacktestSpec {
    pub schema_version: u32,
    pub data_request: DataRequest,
    pub backtest: DepthBacktestSpec,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderDepthBacktestOutcome {
    pub outcome: DepthBacktestOutcome,
    pub provider_id: String,
    pub provider_version: String,
    pub request_id: String,
    pub retry_chain: Vec<String>,
    pub source_hash: String,
    pub depth_path: String,
}

impl ProviderDepthBacktestSpec {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != PROVIDER_DEPTH_BACKTEST_SPEC_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "unsupported ProviderDepthBacktestSpec schema_version {}; expected {}",
                    self.schema_version, PROVIDER_DEPTH_BACKTEST_SPEC_SCHEMA_VERSION
                ),
            ));
        }
        self.data_request
            .query
            .validate()
            .and_then(|()| {
                self.data_request
                    .requirements
                    .validate_for(&self.data_request.query)
            })
            .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error.message))?;
        self.backtest.validate()?;
        let instruments = &self.data_request.query.instrument_set;
        let expected_tier = match self.backtest.tier.as_str() {
            "l1" => Some(qx_provider::MarketDataTier::L1),
            "l2" => Some(qx_provider::MarketDataTier::L2),
            _ => None,
        };
        if self.data_request.query.kind != DataKind::Quote
            || expected_tier.is_none()
            || self.data_request.requirements.required_data_tier != expected_tier
            || instruments.len() != 1
            || self
                .backtest
                .market_spec
                .as_ref()
                .is_some_and(|market| !instruments.contains(&market.instrument.to_string()))
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "Provider depth backtest requires a declared l1/l2 Quote tier matching its one instrument and market_spec",
            ));
        }
        Ok(())
    }
}

/// Fetch validated Quote payloads through the shared registry, freeze their canonical
/// order-book projection, then execute the exact `run_depth_backtest` use case.
pub fn run_provider_depth_backtest(
    registry: &ProviderRegistry,
    spec: &ProviderDepthBacktestSpec,
    context: &RunContext,
) -> Result<ProviderDepthBacktestOutcome, AppError> {
    context.require(CallerCapability::Research, "Provider Tick/OrderBook 回测")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        spec.validate()?;
        let result = registry
            .fetch_request(&spec.data_request)
            .map_err(|error| {
                let (category, message) = match error {
                    RegistryError::NoProvider => (
                        AppErrorCategory::DataUnavailable,
                        "no Provider satisfies the depth backtest data request".to_string(),
                    ),
                    RegistryError::Duplicate(id) => (
                        AppErrorCategory::InvalidInput,
                        format!("duplicate Provider registration: {id}"),
                    ),
                    RegistryError::Invariant(message) => {
                        (AppErrorCategory::InternalInvariant, message)
                    }
                    RegistryError::Provider(error) => {
                        let category = match error.class {
                            ProviderErrorClass::SwitchProvider => AppErrorCategory::DataUnavailable,
                            ProviderErrorClass::ManualIntervention => {
                                AppErrorCategory::PermissionDenied
                            }
                            ProviderErrorClass::Permanent => AppErrorCategory::InvalidInput,
                        };
                        (category, error.message)
                    }
                };
                AppError::new(category, format!("Provider depth fetch failed: {message}"))
            })?;
        let expected_instrument = spec
            .data_request
            .query
            .instrument_set
            .iter()
            .next()
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    "Provider depth request has no instrument",
                )
            })?;
        let mut snapshots = Vec::with_capacity(result.payloads.len());
        for (record, payload) in result.records.iter().zip(&result.payloads) {
            if payload.instrument != *expected_instrument
                || payload.content_type != ORDER_BOOK_SNAPSHOT_CONTENT_TYPE
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "Provider returned a non-snapshot payload or mismatched depth instrument",
                ));
            }
            let snapshot: OrderBookSnapshot =
                serde_json::from_str(&payload.json).map_err(|error| {
                    AppError::new(
                        AppErrorCategory::InvalidInput,
                        format!("Provider OrderBookSnapshot JSON is invalid: {error}"),
                    )
                })?;
            snapshot.validate().map_err(|error| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("Provider OrderBookSnapshot is invalid: {error}"),
                )
            })?;
            if snapshot.instrument.to_string() != payload.instrument
                || snapshot.ts != record.event_time
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "Provider snapshot instrument or timestamp does not match record lineage",
                ));
            }
            if spec.backtest.tier == "l1" && (snapshot.bids.len() != 1 || snapshot.asks.len() != 1)
            {
                return Err(AppError::new(
                    AppErrorCategory::FidelityInsufficient,
                    "L1 Provider snapshots must have exactly one bid and ask level",
                ));
            }
            snapshots.push(snapshot);
        }
        snapshots.sort_by_key(|snapshot| (snapshot.ts, snapshot.sequence));
        if snapshots
            .windows(2)
            .any(|pair| pair[0].ts >= pair[1].ts || pair[0].sequence >= pair[1].sequence)
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "Provider depth snapshots require strictly increasing timestamps and sequences",
            ));
        }
        let source = format!(
            "{}@{}#query={:016x}#request={}#source={:016x}",
            result.provider_id,
            result.provider_version,
            spec.data_request.query.digest(),
            result.request_id,
            result.source_hash
        );
        let frame = DepthFrame {
            schema_version: DepthFrame::SUPPORTED_SCHEMA_VERSION,
            source,
            instrument: snapshots
                .first()
                .ok_or_else(|| {
                    AppError::new(
                        AppErrorCategory::DataUnavailable,
                        "Provider returned no depth snapshots",
                    )
                })?
                .instrument
                .clone(),
            snapshots,
        };
        frame.validate().map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("Provider DepthFrame is invalid: {error}"),
            )
        })?;
        let frame_json = serde_json::to_vec(&frame).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("validated Provider DepthFrame serialization failed: {error}"),
            )
        })?;
        let depth_path = PathBuf::from(&spec.backtest.depth_path);
        freeze(&depth_path, &frame_json)?;
        let outcome = run_depth_backtest(&spec.backtest, context)?;
        Ok(ProviderDepthBacktestOutcome {
            outcome,
            provider_id: result.provider_id,
            provider_version: result.provider_version,
            request_id: result.request_id,
            retry_chain: result.retry_chain,
            source_hash: format!("{:016x}", result.source_hash),
            depth_path: depth_path.to_string_lossy().into_owned(),
        })
    })
    .map_err(|error| attach(error, &correlation_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuiltinStrategySpec, CallerCapability, RunContext};
    use qx_core::{InstrumentId, Price, Quantity, SCALE};
    use qx_provider::{
        DataProvider, DataQuery, DataRequirements, ProviderCapability, ProviderError,
        ProviderPayload, ProviderResult, PROVIDER_RESULT_SCHEMA_VERSION,
    };
    use qx_strategy::BuiltinStrategyKind;
    use qx_xingban::{BookLevel, OrderBookSnapshot};
    use std::collections::BTreeSet;

    struct FixtureProvider {
        capability: ProviderCapability,
        result: ProviderResult,
    }

    impl DataProvider for FixtureProvider {
        fn capability(&self) -> &ProviderCapability {
            &self.capability
        }

        fn fetch(&self, _query: &DataQuery) -> Result<ProviderResult, ProviderError> {
            Ok(self.result.clone())
        }
    }

    fn snapshot(ts: u64) -> OrderBookSnapshot {
        OrderBookSnapshot {
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            ts,
            sequence: ts,
            bids: vec![BookLevel {
                price: Price::from_i64(100 + ts as i64),
                qty: Quantity::from_i64(2),
            }],
            asks: vec![BookLevel {
                price: Price::from_i64(101 + ts as i64),
                qty: Quantity::from_i64(2),
            }],
        }
    }

    fn registry() -> (ProviderRegistry, DataRequest) {
        let capability = ProviderCapability {
            provider_id: "fixture-depth".into(),
            version: "v1".into(),
            data_kinds: [DataKind::Quote].into_iter().collect(),
            data_tiers: [qx_provider::MarketDataTier::L1].into_iter().collect(),
            asset_classes: ["crypto".into()].into_iter().collect(),
            frequencies: ["tick".into()].into_iter().collect(),
            auth_scope: "public".into(),
            rate_limit_per_second: 100,
            freshness_seconds: 0,
            historical_start: 1,
            historical_end: 3,
            realtime: false,
            priority: 0,
            quality_score: 100,
            cost_score: 0,
        };
        let instrument = "BTCUSDT.BINANCE";
        let payloads = (1..=3)
            .map(|ts| {
                let book = snapshot(ts);
                (
                    ts,
                    ts,
                    ProviderPayload::new_json(
                        instrument,
                        ORDER_BOOK_SNAPSHOT_CONTENT_TYPE,
                        serde_json::to_string(&book).unwrap(),
                    ),
                )
            })
            .collect();
        let result =
            ProviderResult::from_payloads("fixture-depth", "v1", "depth-request-1", 3, payloads)
                .unwrap();
        assert_eq!(result.schema_version, PROVIDER_RESULT_SCHEMA_VERSION);
        let mut registry = ProviderRegistry::new();
        registry
            .register(Box::new(FixtureProvider { capability, result }))
            .unwrap();
        let request = DataRequest::new(
            DataQuery {
                kind: DataKind::Quote,
                asset_class: "crypto".into(),
                instrument_set: [instrument.to_string()]
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                field_set: BTreeSet::new(),
                frequency: "tick".into(),
                adjustment: "none".into(),
                quality_policy: "strict".into(),
                start: 1,
                end: 3,
                as_of: None,
            },
            DataRequirements {
                required_data_tier: Some(qx_provider::MarketDataTier::L1),
                ..DataRequirements::default()
            },
        );
        (registry, request)
    }
    /// 三份夹具共用的深度 spec：数据层用例不声明任何门面口径，
    /// 所以「来源标签 / 声明即拒」那组字段一律 `None` —— 用例侧留空即代表
    /// 「没有门面给过这些口径」，而不是「门面给了但被丢了」。
    fn depth_spec(
        run_id: &str,
        depth_path: String,
        output_dir: String,
        strategy_id: &str,
        account_id: Option<String>,
    ) -> DepthBacktestSpec {
        DepthBacktestSpec {
            schema_version: crate::spec::DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
            run_id: run_id.into(),
            depth_path,
            tier: "l1".into(),
            settlement_currency: "USDT".into(),
            initial_cash_raw: 10_000 * SCALE,
            fee_bps: Some(0),
            latency_snapshots: 0,
            queue_position_bps: 0,
            queue_position_model: qx_xingban::QueuePositionModel::Disabled,
            market_impact_bps: 0,
            output_dir,
            strategy: BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, strategy_id),
            market_spec: None,
            order_policy: None,
            market_making: None,
            account_id,
            portfolio: None,
            manifest_run_id: None,
            execution_cost_rules: None,
            execution_cost_source: None,
            risk_rules: None,
            allow_short: None,
            declared_ashare_rules: None,
            declared_fill_model: None,
            declared_bar_volume_participation_bps: None,
            quality_manifest_path: None,
            runtime_config_path: None,
            market_spec_path: None,
            market_spec_source: None,
            account_base_source: None,
            signal_source: None,
            signal_declared_unused: None,
        }
    }

    #[test]
    fn provider_depth_data_is_frozen_with_lineage_then_runs_shared_engine() {
        let (registry, data_request) = registry();
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("qx-provider-depth-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let depth_path = root.join("depth.json");
        let backtest = depth_spec(
            "provider-depth-e2e",
            depth_path.to_string_lossy().into_owned(),
            root.join("runs").to_string_lossy().into_owned(),
            "provider-sma",
            Some("provider-test".into()),
        );
        let spec = ProviderDepthBacktestSpec {
            schema_version: PROVIDER_DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
            data_request,
            backtest,
        };
        let mut mismatched = spec.clone();
        mismatched.backtest.tier = "l2".into();
        assert!(mismatched.validate().is_err());
        let outcome = run_provider_depth_backtest(
            &registry,
            &spec,
            &RunContext::new(CallerCapability::Research, "provider-depth-e2e"),
        )
        .unwrap();
        let frozen = DepthFrame::from_json(&std::fs::read_to_string(&depth_path).unwrap()).unwrap();
        assert_eq!(outcome.provider_id, "fixture-depth");
        assert_eq!(outcome.retry_chain, ["fixture-depth"]);
        assert!(frozen.source.contains("#source="));
        assert_eq!(frozen.snapshots.len(), 3);
        assert_eq!(outcome.outcome.run_id, "provider-depth-e2e");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn provider_depth_spec_rejects_bar_query_before_provider_access() {
        let (registry, mut request) = registry();
        request.query.kind = DataKind::Bar;
        let spec = ProviderDepthBacktestSpec {
            schema_version: PROVIDER_DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
            data_request: request,
            backtest: depth_spec(
                "invalid-provider-depth",
                "target/depth.json".into(),
                "target/provider-depth-invalid".into(),
                "invalid",
                None,
            ),
        };
        let error = run_provider_depth_backtest(
            &registry,
            &spec,
            &RunContext::new(CallerCapability::Research, "invalid-provider-depth"),
        )
        .unwrap_err();
        assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    }

    #[test]
    fn provider_depth_spec_rejects_unknown_schema_before_provider_access() {
        let (registry, data_request) = registry();
        let spec = ProviderDepthBacktestSpec {
            schema_version: PROVIDER_DEPTH_BACKTEST_SPEC_SCHEMA_VERSION + 1,
            data_request,
            backtest: depth_spec(
                "future-provider-depth",
                "target/future-depth.json".into(),
                "target/future-provider-depth".into(),
                "future",
                None,
            ),
        };
        let error = run_provider_depth_backtest(
            &registry,
            &spec,
            &RunContext::new(CallerCapability::Research, "future-provider-depth"),
        )
        .unwrap_err();
        assert_eq!(error.category(), AppErrorCategory::InvalidInput);
        assert!(error.message().contains("schema_version"));
        assert!(error.message().contains("expected 1"));
    }
}
