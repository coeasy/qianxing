//! Application-owned verification for shared-account multi-market report facts.

#[cfg(test)]
use crate::report_read::{event_fill_identity, update_fill_sequence_digest};
#[cfg(test)]
use crate::StrategyArtifactIdentity;
use crate::{AppError, AppErrorCategory, MultiMarketBacktestSpec, MultiMarketDepthBacktestSpec};
use qx_core::ExactDecimal;
#[cfg(test)]
use qx_core::Fnv1a;
use serde_json::Value;
use std::collections::BTreeMap;

mod identity;
pub(crate) use identity::multi_market_strategy_set_fingerprint;
pub use identity::{
    multi_market_depth_input_set_identity_v1, multi_market_depth_spec_fingerprint_v1,
    multi_market_input_set_identity_v1, multi_market_spec_fingerprint_v1,
    MultiMarketDepthInputIdentityV1, MultiMarketInputIdentityV1, MultiMarketInputSetIdentityV1,
};
mod inputs;
mod pnl_contributions;
pub use inputs::{
    validate_multi_market_quality_manifest_set_v1, verify_multi_market_bar_input_v1,
    verify_multi_market_depth_input_v1, VerifiedMultiMarketBarInputV1,
    VerifiedMultiMarketDepthInputV1,
};
mod sidecars;
pub use pnl_contributions::{
    verify_multi_market_pnl_contributions_v1, verify_multi_market_pnl_contributions_with_exact_v1,
};
pub use sidecars::{
    verify_multi_market_bar_sidecars_from_event_report_v1,
    verify_multi_market_bar_sidecars_from_event_report_with_exact_v1,
    verify_multi_market_bar_sidecars_v1, verify_multi_market_bar_sidecars_with_exact_v1,
    verify_multi_market_event_fill_sidecar_v1, verify_multi_market_legacy_event_fill_sidecar_v1,
    VerifiedMultiMarketBarSidecarsV1,
};

pub const MAX_MULTI_MARKET_PNL_SAMPLES: usize = qx_xingban::MAX_INSTRUMENT_EQUITY_SAMPLES;

/// Bind quality-manifest identity to metadata-bearing snapshot context when available.
/// Legacy manifest-only runs keep their existing identity; a DatasetSnapshotV1 changes the
/// canonical quality identity so role, calendar, adjustment policy, coverage, and gaps cannot
/// drift without changing the run fingerprint.
pub(crate) fn dataset_quality_identity_fingerprint(
    manifest_fingerprint: &str,
    snapshot_sha256: Option<&str>,
) -> String {
    match snapshot_sha256 {
        Some(snapshot_sha256) => qx_strategy::sha256_hex(
            format!("dataset-quality-snapshot-v1\n{manifest_fingerprint}\n{snapshot_sha256}")
                .as_bytes(),
        ),
        None => manifest_fingerprint.to_owned(),
    }
}

/// A verified point in an instrument's contribution to shared portfolio equity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifiedPnlContributionSample {
    pub equity_index: u64,
    pub ts: u64,
    pub pnl_contribution_raw: i128,
    pub pnl_contribution_exact: Option<ExactDecimal>,
}

/// Replayed and cross-checked contribution curves keyed by canonical instrument ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMultiMarketPnlContributions {
    pub total_equity_points: u64,
    pub curves: BTreeMap<String, Vec<VerifiedPnlContributionSample>>,
}

/// Verify contribution sidecar points against the shared equity curve and replayed ledger.
///
/// The sidecar must include exactly one curve for every market in `spec`. Every sampled
/// portfolio point must be reconstructed by summing instrument contributions, and the final
/// sample is independently revalued from the replayed ledger and frozen instrument specs.
pub(super) fn raw_i128(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
        .or_else(|| value.as_str().and_then(|raw| raw.parse::<i128>().ok()))
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::Conflict, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MultiMarketBacktestMarket, MultiMarketDepthBacktestMarket};
    use qx_core::{InstrumentId, Ledger, Price, TradingInstrumentSpec};

    fn spec() -> MultiMarketBacktestSpec {
        let market = |instrument: &str| MultiMarketBacktestMarket {
            instrument: instrument.to_owned(),
            bars_path: "unused.json".into(),
            dataset_manifest_path: None,
            priority: 0,
            market_spec: None,
            asset_class: None,
            fee_schedule: None,
            order_policy: None,
            margin_model: None,
            funding_bps: 0,
            funding_events: Vec::new(),
            strategies: Vec::new(),
        };
        MultiMarketBacktestSpec {
            schema_version: crate::MULTI_MARKET_BACKTEST_SCHEMA_VERSION,
            run_id: "pnl-report-test".into(),
            account_id: "report-account".into(),
            settlement_currency: "USDT".into(),
            initial_cash_raw: 100,
            initial_cash_exact: None,
            seed: 1,
            output_dir: "unused".into(),
            markets: vec![market("BTCUSDT.BINANCE"), market("ETHUSDT.BINANCE")],
        }
    }

    fn depth_spec() -> MultiMarketDepthBacktestSpec {
        let market = |instrument: &str, tier: &str, dataset_manifest_path| {
            let instrument_id = InstrumentId::parse(instrument).unwrap();
            MultiMarketDepthBacktestMarket {
                instrument: instrument.to_owned(),
                depth_path: format!("{instrument}.depth.json"),
                dataset_manifest_path,
                data_tier: Some(tier.to_owned()),
                priority: 0,
                market_spec: TradingInstrumentSpec::spot_default(&instrument_id, "USDT"),
                fee_bps: 0,
                funding_bps: 0,
                funding_events: Vec::new(),
                latency_snapshots: 0,
                queue_position_bps: 0,
                queue_position_model: Default::default(),
                market_impact_bps: 0,
                strategy: crate::BuiltinStrategySpec::new(
                    qx_strategy::BuiltinStrategyKind::SmaCross,
                    format!("strategy-{instrument}"),
                ),
                order_policy: None,
            }
        };
        MultiMarketDepthBacktestSpec {
            schema_version: crate::MULTI_MARKET_DEPTH_BACKTEST_SCHEMA_VERSION,
            run_id: "multi-market-depth-identity".into(),
            account_id: "report-account".into(),
            settlement_currency: "USDT".into(),
            initial_cash_raw: 100,
            initial_cash_exact: None,
            output_dir: "unused".into(),
            markets: vec![
                market("BTCUSDT.BINANCE", "l1", Some("btc-quality.json".into())),
                market("ETHUSDT.BINANCE", "l2", None),
            ],
        }
    }

    fn environment() -> (
        Ledger,
        BTreeMap<InstrumentId, TradingInstrumentSpec>,
        BTreeMap<InstrumentId, Price>,
    ) {
        let markets = ["BTCUSDT.BINANCE", "ETHUSDT.BINANCE"]
            .into_iter()
            .map(|name| InstrumentId::parse(name).unwrap())
            .collect::<Vec<_>>();
        let specs = markets
            .iter()
            .map(|instrument| {
                (
                    instrument.clone(),
                    TradingInstrumentSpec::spot_default(instrument, "USDT"),
                )
            })
            .collect();
        let marks = markets
            .into_iter()
            .map(|instrument| (instrument, Price::from_raw(10)))
            .collect();
        (Ledger::new(), specs, marks)
    }

    fn sidecar(btc_samples: &[(u64, u64, &str)], eth_samples: &[(u64, u64, &str)]) -> Value {
        let rows = |samples: &[(u64, u64, &str)]| {
            samples
                .iter()
                .map(|(equity_index, ts, amount)| {
                    serde_json::json!({
                        "equity_index": equity_index,
                        "ts": ts,
                        "pnl_contribution_raw": amount
                    })
                })
                .collect::<Vec<_>>()
        };
        serde_json::json!({
            "BTCUSDT.BINANCE": rows(btc_samples),
            "ETHUSDT.BINANCE": rows(eth_samples)
        })
    }

    #[test]
    fn multi_market_contributions_reconcile_every_sample_and_final_ledger_value() {
        let spec = spec();
        let (ledger, market_specs, marks) = environment();
        let curves = sidecar(&[(0, 10, "0"), (1, 20, "0")], &[(0, 10, "0"), (1, 20, "0")]);
        let equity = [serde_json::json!([10, 100]), serde_json::json!([20, 100])];
        let verified = verify_multi_market_pnl_contributions_v1(
            &curves,
            &equity,
            &spec,
            &ledger,
            &market_specs,
            &marks,
        )
        .expect("contributions should reconcile");
        assert_eq!(verified.total_equity_points, 2);
        assert_eq!(verified.curves.len(), 2);
        assert_eq!(
            verified.curves["BTCUSDT.BINANCE"][1].pnl_contribution_raw,
            0
        );
    }

    #[test]
    fn multi_market_exact_contributions_preserve_subnano_cash_and_replay_check() {
        let spec = spec();
        let (mut ledger, market_specs, marks) = environment();
        ledger
            .deposit("report-account", "USDT", qx_core::Money::from_raw(100), 1)
            .unwrap();
        let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let amount = ExactDecimal::parse("0.0000000001").unwrap();
        ledger
            .apply_entry(qx_core::LedgerEntry {
                id: 1,
                account_id: "report-account".into(),
                currency: "USDT".into(),
                kind: qx_core::LedgerEntryKind::TradeCash,
                amount: qx_core::Money::ZERO,
                instrument: Some(btc),
                quantity: qx_core::Quantity::ZERO,
                price: None,
                order_id: None,
                ts: 2,
                multiplier: 1,
                position_side: None,
                leverage: None,
                exact_amounts: Some(qx_core::ExactLedgerAmounts {
                    amount,
                    quantity: ExactDecimal::ZERO,
                    price: None,
                    contract_size: None,
                    inverse: None,
                }),
            })
            .unwrap();
        let curves = serde_json::json!({
            "BTCUSDT.BINANCE": [
                {"equity_index": 0, "ts": 10, "pnl_contribution_raw": "0", "pnl_contribution_exact": "0"},
                {"equity_index": 1, "ts": 20, "pnl_contribution_raw": "0", "pnl_contribution_exact": "0.0000000001"}
            ],
            "ETHUSDT.BINANCE": [
                {"equity_index": 0, "ts": 10, "pnl_contribution_raw": "0", "pnl_contribution_exact": "0"},
                {"equity_index": 1, "ts": 20, "pnl_contribution_raw": "0", "pnl_contribution_exact": "0"}
            ]
        });
        let equity = [serde_json::json!([10, 100]), serde_json::json!([20, 100])];
        let exact_equity = [
            serde_json::json!({"equity_index": 0, "ts": 10, "equity_exact": "0.0000001"}),
            serde_json::json!({"equity_index": 1, "ts": 20, "equity_exact": "0.0000001001"}),
        ];

        let verified = verify_multi_market_pnl_contributions_with_exact_v1(
            &curves,
            &equity,
            Some(&exact_equity),
            &spec,
            &ledger,
            &market_specs,
            &marks,
        )
        .unwrap();

        assert_eq!(
            verified.curves["BTCUSDT.BINANCE"][1].pnl_contribution_exact,
            Some(amount)
        );
        let mut inconsistent = exact_equity;
        inconsistent[1]["equity_exact"] = Value::String("0.0000001002".into());
        assert!(verify_multi_market_pnl_contributions_with_exact_v1(
            &curves,
            &equity,
            Some(&inconsistent),
            &spec,
            &ledger,
            &market_specs,
            &marks,
        )
        .is_err());
    }

    #[test]
    fn multi_market_contributions_reject_a_non_conserving_equity_sample() {
        let spec = spec();
        let (ledger, market_specs, marks) = environment();
        let curves = sidecar(&[(0, 10, "1"), (1, 20, "0")], &[(0, 10, "0"), (1, 20, "0")]);
        let equity = [serde_json::json!([10, 100]), serde_json::json!([20, 100])];
        assert!(verify_multi_market_pnl_contributions_v1(
            &curves,
            &equity,
            &spec,
            &ledger,
            &market_specs,
            &marks,
        )
        .is_err());
    }

    #[test]
    fn multi_market_contributions_reject_terminal_values_not_in_the_replayed_ledger() {
        let spec = spec();
        let (ledger, market_specs, marks) = environment();
        let curves = sidecar(
            &[(0, 10, "0"), (1, 20, "1")],
            &[(0, 10, "0"), (1, 20, "-1")],
        );
        let equity = [serde_json::json!([10, 100]), serde_json::json!([20, 100])];
        assert!(verify_multi_market_pnl_contributions_v1(
            &curves,
            &equity,
            &spec,
            &ledger,
            &market_specs,
            &marks,
        )
        .is_err());
    }

    #[test]
    fn multi_market_spec_fingerprint_ignores_local_paths_and_sorts_strategy_identities() {
        let spec = spec();
        let mut relocated = spec.clone();
        relocated.output_dir = "another/output".into();
        relocated.markets[0].bars_path = "another/bars.json".into();
        relocated.markets[0].dataset_manifest_path = Some("another/quality.json".into());
        let alpha = StrategyArtifactIdentity {
            strategy_id: "alpha".into(),
            strategy_version: "v1".into(),
            source_sha256: "a".repeat(64),
        };
        let beta = StrategyArtifactIdentity {
            strategy_id: "beta".into(),
            strategy_version: "v2".into(),
            source_sha256: "b".repeat(64),
        };
        assert_eq!(
            multi_market_spec_fingerprint_v1(&spec, &[alpha.clone(), beta.clone()]).unwrap(),
            multi_market_spec_fingerprint_v1(&relocated, &[beta, alpha]).unwrap(),
        );
        assert_eq!(
            multi_market_spec_fingerprint_v1(&spec, &[]).unwrap(),
            multi_market_spec_fingerprint_v1(&relocated, &[]).unwrap(),
        );
    }

    #[test]
    fn multi_market_input_identity_uses_spec_order_and_quality_components() {
        let mut spec = spec();
        spec.markets[1].dataset_manifest_path = Some("quality.json".into());
        let identity = multi_market_input_set_identity_v1(
            &spec,
            &[
                MultiMarketInputIdentityV1 {
                    instrument: "ETHUSDT.BINANCE".into(),
                    bar_fingerprint: "eth-bars".into(),
                    quality_fingerprint: Some("eth-quality".into()),
                },
                MultiMarketInputIdentityV1 {
                    instrument: "BTCUSDT.BINANCE".into(),
                    bar_fingerprint: "btc-bars".into(),
                    quality_fingerprint: None,
                },
            ],
        )
        .expect("the exact configured market set should produce an identity");
        let mut expected = Fnv1a::new();
        expected.write_text("BTCUSDT.BINANCE");
        expected.write_text("btc-bars");
        expected.write_text("ETHUSDT.BINANCE");
        expected.write_text("eth-bars");
        expected.write_text("eth-quality");
        assert_eq!(
            identity.data_fingerprint,
            format!("{:016x}", expected.finish())
        );
        assert_eq!(identity.input_components.len(), 3);
        assert!(multi_market_input_set_identity_v1(
            &spec,
            &[MultiMarketInputIdentityV1 {
                instrument: "BTCUSDT.BINANCE".into(),
                bar_fingerprint: "btc-bars".into(),
                quality_fingerprint: None,
            }]
        )
        .is_err());
    }

    #[test]
    fn multi_market_depth_input_identity_is_shared_and_path_independent() {
        let mut spec = depth_spec();
        let inputs = [
            MultiMarketDepthInputIdentityV1 {
                instrument: "ETHUSDT.BINANCE".into(),
                data_tier: "l2".into(),
                source: "eth-feed".into(),
                input_hash: 22,
                start_ts_ms: 1,
                end_ts_ms: 2,
                quality_fingerprint: None,
            },
            MultiMarketDepthInputIdentityV1 {
                instrument: "BTCUSDT.BINANCE".into(),
                data_tier: "l1".into(),
                source: "btc-feed".into(),
                input_hash: 11,
                start_ts_ms: 10,
                end_ts_ms: 20,
                quality_fingerprint: Some("btc-quality".into()),
            },
        ];
        let identity = multi_market_depth_input_set_identity_v1(&spec, &inputs)
            .expect("the exact configured depth input set should produce an identity");
        let mut expected = Fnv1a::new();
        expected.write_text("BTCUSDT.BINANCE");
        expected.write_text("l1");
        expected.write_text("btc-feed");
        expected.write_u64(11);
        expected.write_text("btc-quality");
        expected.write_text("ETHUSDT.BINANCE");
        expected.write_text("l2");
        expected.write_text("eth-feed");
        expected.write_u64(22);
        assert_eq!(
            identity.data_fingerprint,
            format!("{:016x}", expected.finish())
        );
        assert_eq!(identity.input_components.len(), 3);
        assert_eq!(
            identity.input_components["depthframe:BTCUSDT.BINANCE"],
            "depth:l1:btc-feed:000000000000000b"
        );

        spec.output_dir = "relocated-output".into();
        spec.markets[0].depth_path = "relocated-btc.json".into();
        spec.markets[0].dataset_manifest_path = Some("relocated-quality.json".into());
        assert_eq!(
            identity.data_fingerprint,
            multi_market_depth_input_set_identity_v1(&spec, &inputs)
                .unwrap()
                .data_fingerprint
        );

        let mut mismatched_tier = inputs.clone();
        mismatched_tier[1].data_tier = "l2".into();
        assert!(multi_market_depth_input_set_identity_v1(&spec, &mismatched_tier).is_err());
        assert!(multi_market_depth_input_set_identity_v1(&spec, &inputs[..1]).is_err());
    }

    #[test]
    fn multi_market_depth_spec_fingerprint_sorts_and_binds_strategy_identities() {
        let spec = depth_spec();
        let alpha = StrategyArtifactIdentity {
            strategy_id: "strategy-BTCUSDT.BINANCE".into(),
            strategy_version: "1".into(),
            source_sha256: "a".repeat(64),
        };
        let beta = StrategyArtifactIdentity {
            strategy_id: "strategy-ETHUSDT.BINANCE".into(),
            strategy_version: "2".into(),
            source_sha256: "b".repeat(64),
        };
        let mut identity_spec = spec.clone();
        identity_spec.output_dir.clear();
        for market in &mut identity_spec.markets {
            market.depth_path.clear();
            market.dataset_manifest_path = None;
        }
        let mut legacy_shape_hasher = Fnv1a::new();
        legacy_shape_hasher.write_text(&identity_spec.to_json().unwrap());
        for identity in [&alpha, &beta] {
            legacy_shape_hasher.write_text(&identity.strategy_id);
            legacy_shape_hasher.write_text(&identity.strategy_version);
            legacy_shape_hasher.write_text(&identity.source_sha256);
        }
        assert_eq!(
            multi_market_depth_spec_fingerprint_v1(&spec, &[alpha.clone(), beta.clone()]).unwrap(),
            format!("{:016x}", legacy_shape_hasher.finish())
        );
        assert_eq!(
            multi_market_depth_spec_fingerprint_v1(&spec, &[alpha.clone(), beta.clone()]).unwrap(),
            multi_market_depth_spec_fingerprint_v1(&spec, &[beta.clone(), alpha.clone()]).unwrap()
        );
        let mut relocated = spec.clone();
        relocated.output_dir = "relocated-output".into();
        relocated.markets[0].depth_path = "relocated-btc.json".into();
        relocated.markets[0].dataset_manifest_path = Some("relocated-quality.json".into());
        assert_eq!(
            multi_market_depth_spec_fingerprint_v1(&spec, &[alpha.clone(), beta.clone()]).unwrap(),
            multi_market_depth_spec_fingerprint_v1(&relocated, &[beta.clone(), alpha.clone()])
                .unwrap()
        );
        let mut unknown = alpha.clone();
        unknown.strategy_id = "orphan-strategy".into();
        assert!(multi_market_depth_spec_fingerprint_v1(&spec, &[unknown]).is_err());
        assert!(multi_market_depth_spec_fingerprint_v1(&spec, &[alpha.clone(), alpha]).is_err());
    }

    #[test]
    fn multi_market_bar_sidecars_reconcile_metrics_and_filled_events() {
        let spec = spec();
        let fill = serde_json::json!({
            "ts": 30,
            "order_id": 7,
            "qty_raw": "2",
            "price_raw": "5",
            "fee_raw": "1",
            "strategy_id": "sma"
        });
        let events = [serde_json::json!({
            "seq": 9,
            "kind": { "Filled": { "fill": fill.clone() } }
        })];
        let equity = serde_json::json!([[10, 100], [20, 110], [30, 105]]);
        let fills = serde_json::json!([fill]);
        let verified = verify_multi_market_bar_sidecars_v1(
            &equity, &fills, &events, &spec, 3, 1, 105, 500, 454,
        )
        .expect("equity metrics and Filled facts should reconcile");
        assert_eq!(verified.equity_points, 3);
        assert_eq!(verified.first_equity_raw, 100);
        assert_eq!(verified.final_equity_raw, 105);
        assert_eq!(verified.return_bps, 500);
        assert_eq!(verified.max_drawdown_bps, 454);
        assert_eq!(verified.fill_rows, 1);

        let tampered_fills = serde_json::json!([]);
        assert!(verify_multi_market_bar_sidecars_v1(
            &equity,
            &tampered_fills,
            &events,
            &spec,
            3,
            1,
            105,
            500,
            454,
        )
        .is_err());
        assert!(verify_multi_market_bar_sidecars_v1(
            &equity, &fills, &events, &spec, 3, 1, 105, 500, 400,
        )
        .is_err());
    }

    #[test]
    fn raw_event_report_sidecars_keep_the_legacy_verification_path() {
        let spec = spec();
        let fill = qx_core::Fill {
            order_id: 7,
            qty: qx_core::Quantity::from_raw(2),
            price: qx_core::Price::from_raw(5),
            fee: qx_core::Money::from_raw(1),
            ts: 30,
            account_id: String::new(),
            strategy_id: Some("sma".into()),
            ..Default::default()
        };
        let mut digest = qx_strategy::file_digest::Sha256Accumulator::new();
        digest.update(b"qx-event-fill-sequence-v1\0");
        update_fill_sequence_digest(&mut digest, &event_fill_identity(&fill))
            .expect("fill identity digest");
        let fills = serde_json::json!([serde_json::to_value(&fill).unwrap()]);
        let equity = serde_json::json!([[10, 100], [20, 110], [30, 105]]);

        let verified = verify_multi_market_bar_sidecars_from_event_report_v1(
            &equity,
            &fills,
            1,
            &digest.finish_hex(),
            &spec,
            3,
            105,
            500,
            454,
        )
        .expect("legacy raw EventReport sidecars should verify");
        assert_eq!(verified.fill_rows, 1);
        assert_eq!(verified.final_equity_raw, 105);
    }

    #[test]
    fn legacy_multi_market_depth_fills_match_the_inline_filled_event_rows() {
        let fill = serde_json::json!({
            "ts": 30,
            "order_id": 7,
            "qty_raw": "2",
            "price_raw": "5",
            "fee_raw": "1",
            "strategy_id": "sma"
        });
        let events = [serde_json::json!({
            "seq": 9,
            "kind": { "Filled": { "fill": fill.clone() } }
        })];
        let fills = serde_json::json!([fill]);

        verify_multi_market_legacy_event_fill_sidecar_v1(&fills, &events, 1)
            .expect("legacy sidecar rows should match embedded Filled events");
        assert!(verify_multi_market_legacy_event_fill_sidecar_v1(&fills, &events, 2).is_err());
        assert!(verify_multi_market_legacy_event_fill_sidecar_v1(
            &serde_json::json!([]),
            &events,
            1
        )
        .is_err());
    }

    fn bar_frame_fixture() -> (String, qx_datastruct::BarFrame) {
        let payload = serde_json::json!({
            "schema_version": 1,
            "instrument": "BTCUSDT.BINANCE",
            "source": "multi-market-report-test",
            "ts": [1_000, 2_000],
            "open_raw": [10, 11],
            "high_raw": [11, 12],
            "low_raw": [9, 10],
            "close_raw": [10, 11],
            "volume_raw": [100, 110]
        })
        .to_string();
        let frame = qx_datastruct::BarFrame::from_json(&payload).expect("BarFrame fixture");
        (payload, frame)
    }

    #[test]
    fn multi_market_bar_input_reconciles_bar_identity_and_quality_manifest() {
        let (frame_payload, frame) = bar_frame_fixture();
        let fingerprint = qx_data::fingerprint_bar_frame(&frame).expect("BarFrame fingerprint");
        let recorded_market = serde_json::json!({
            "instrument": "BTCUSDT.BINANCE",
            "content_hash": fingerprint,
            "row_count": 2
        });
        let mut configured = spec().markets[0].clone();
        let bar_only = verify_multi_market_bar_input_v1(
            &configured,
            &recorded_market,
            &frame_payload,
            &frame,
            None,
            None,
        )
        .expect("BarFrame identity should reconcile without optional quality evidence");
        assert_eq!(bar_only.quality_status, "not_provided");
        assert_eq!(bar_only.row_count, 2);
        assert_eq!(bar_only.source_end_ts, 2_000);
        assert_eq!(bar_only.final_close_raw, 11);

        configured.dataset_manifest_path = Some("quality.json".into());
        let frame_sha256 = qx_data::sha256_hex_bytes(frame_payload.as_bytes());
        let manifest = qx_data::DatasetManifestV2 {
            manifest_version: qx_data::DATASET_MANIFEST_V2_SCHEMA_VERSION,
            dataset: qx_data::DatasetManifest {
                dataset_id: "multi-market-test".into(),
                version: "1".into(),
                source: "test-provider".into(),
                fingerprint: qx_data::fingerprint_bar_frame(&frame).unwrap(),
                schema_version: qx_datastruct::BAR_FRAME_JSON_SCHEMA_VERSION,
                start_timestamp: 1_000,
                end_timestamp: 2_000,
            },
            instrument: "BTCUSDT.BINANCE".into(),
            timezone: "UTC".into(),
            tier: qx_data::DatasetTier::Bar,
            provider_version: qx_data::BARFRAME_DATASET_VERSION.into(),
            content_hash: frame_sha256,
            quality_report: qx_data::DatasetQualityReport {
                row_count: 2,
                duplicate_rows: 0,
                out_of_order_rows: 0,
                missing_intervals: 0,
                interval_ms: Some(1_000),
                acquisition_cutoff_ms: None,
                excluded_unclosed_rows: None,
                timezone: "UTC".into(),
                corporate_action_coverage: false,
                usable_tiers: vec!["bar".into()],
            },
            source_lineage: qx_data::DatasetSourceLineage {
                primary_source: "test-provider".into(),
                backup_sources: Vec::new(),
            },
        };
        let manifest_payload = manifest.to_json().expect("manifest JSON");
        let manifest_fingerprint = manifest.fingerprint().expect("manifest fingerprint");
        let quality_record = serde_json::json!({
            "manifest_path": "quality.json",
            "manifest_sha256": qx_data::sha256_hex_bytes(manifest_payload.as_bytes()),
            "manifest_fingerprint": manifest_fingerprint,
            "quality_report": manifest.quality_report
        });
        let verified = verify_multi_market_bar_input_v1(
            &configured,
            &recorded_market,
            &frame_payload,
            &frame,
            Some(&quality_record),
            Some(&manifest_payload),
        )
        .expect("BarFrame and DatasetManifestV2 should reconcile");
        assert_eq!(verified.quality_status, "verified");
        assert!(verified.quality_fingerprint.is_some());

        let mut tampered_record = quality_record;
        tampered_record["manifest_sha256"] = serde_json::json!("0".repeat(64));
        assert!(verify_multi_market_bar_input_v1(
            &configured,
            &recorded_market,
            &frame_payload,
            &frame,
            Some(&tampered_record),
            Some(&manifest_payload),
        )
        .is_err());
    }

    #[test]
    fn multi_market_quality_records_reject_missing_or_orphan_market_entries() {
        let mut spec = spec();
        spec.markets[0].dataset_manifest_path = Some("quality.json".into());
        let matching = serde_json::json!({"BTCUSDT.BINANCE": {}});
        assert!(validate_multi_market_quality_manifest_set_v1(&spec, &matching).is_ok());
        assert!(
            validate_multi_market_quality_manifest_set_v1(&spec, &serde_json::json!({})).is_err()
        );
        assert!(validate_multi_market_quality_manifest_set_v1(
            &spec,
            &serde_json::json!({"BTCUSDT.BINANCE": {}, "UNKNOWN.BINANCE": {}})
        )
        .is_err());
    }
}
