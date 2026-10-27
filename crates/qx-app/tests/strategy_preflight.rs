mod common;

use common::{scratch, write_frame};
use qx_app::{
    validate_strategy_execution_preflight_v1, AppErrorCategory, CallerCapability, RunContext,
    StrategyExecutionPreflightSpecV1, StrategyPreflightPurpose,
};
use serde_json::{json, Value};

fn research() -> RunContext {
    RunContext::new(CallerCapability::Research, "strategy-preflight-test")
}

fn complete_manifest(warmup_bars: u64) -> Value {
    json!({
        "schema_version": 1,
        "kind": "qianxing-python-strategy-manifest",
        "strategy_id": "sample-sma",
        "strategy_version": "1.0.0",
        "entrypoint": "create_strategy(spec)",
        "parameters": {
            "lookback": {
                "type": "integer",
                "required": true,
                "minimum": 2,
                "maximum": 100
            }
        },
        "instrument_requirements": {
            "asset_classes": ["crypto"],
            "products": ["spot"],
            "venues": ["binance"],
            "requires_explicit_spec": true
        },
        "data_level": "bar",
        "warmup_bars": warmup_bars,
        "dependencies": [],
        "completeness": "complete",
        "source_sha256": "0".repeat(64),
        "bundle_sha256": "1".repeat(64)
    })
}

fn complete_depth_manifest(data_level: &str, warmup_bars: u64) -> Value {
    json!({
        "schema_version": 1,
        "kind": "qianxing-python-strategy-manifest",
        "strategy_id": "sample-depth",
        "strategy_version": "1.0.0",
        "entrypoint": "create_strategy(spec)",
        "parameters": {
            "fast_window": {
                "type": "integer",
                "required": true,
                "minimum": 2,
                "maximum": 100
            }
        },
        "instrument_requirements": {
            "asset_classes": [],
            "products": [],
            "venues": [],
            "requires_explicit_spec": false
        },
        "data_level": data_level,
        "warmup_bars": warmup_bars,
        "dependencies": [],
        "completeness": "complete",
        "source_sha256": "0".repeat(64),
        "bundle_sha256": "1".repeat(64)
    })
}

fn depth_strategy_spec(tier: &str) -> Value {
    json!({
        "schema_version": 1,
        "run_id": "depth-run",
        "depth_path": "depth.jsonl",
        "tier": tier,
        "strategy": {
            "kind": "sma_cross",
            "strategy_id": "sample-depth",
            "quantity_raw": 1,
            "fast_window": 5,
            "slow_window": 10
        }
    })
}

fn single_market_spec(bars_path: &str, lookback: u64) -> Value {
    json!({
        "instrument": "BTCUSDT.BINANCE",
        "bars_path": bars_path,
        "market_spec": {"product": "spot", "exchange_id": "BINANCE"},
        "asset_class": "Crypto",
        "parameters": {"lookback": lookback}
    })
}

fn preflight(manifest: Value, execution_spec: Value) -> StrategyExecutionPreflightSpecV1 {
    StrategyExecutionPreflightSpecV1 {
        schema_version: 1,
        strategy_manifest: manifest,
        execution_spec,
        purpose: StrategyPreflightPurpose::Run,
        target_strategy_id: None,
        split_plan: None,
    }
}

#[test]
fn complete_strategy_preflight_returns_rust_verified_dataset_identity() {
    let root = scratch("strategy-preflight-complete");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 12);
    let result = validate_strategy_execution_preflight_v1(
        &preflight(complete_manifest(4), single_market_spec(&bars, 12)),
        &research(),
    )
    .expect("complete manifest should pass against matching market and Bars");

    assert_eq!(result.strategy_id, "sample-sma");
    assert_eq!(result.strategy_version, "1.0.0");
    assert_eq!(result.completeness, "complete");
    assert_eq!(result.checked_markets.len(), 1);
    assert_eq!(result.checked_markets[0].instrument, "BTCUSDT.BINANCE");
    assert_eq!(result.checked_markets[0].rows, 12);
    assert!(result.checked_markets[0]
        .content_fingerprint
        .starts_with("barframe:"));
}

#[test]
fn complete_tick_and_orderbook_strategy_preflight_match_the_depth_tier() {
    for (level, tier) in [("tick", "l1"), ("orderbook", "l2")] {
        let result = validate_strategy_execution_preflight_v1(
            &preflight(complete_depth_manifest(level, 0), depth_strategy_spec(tier)),
            &research(),
        )
        .expect("a depth strategy manifest matching the tier should pass preflight");
        assert_eq!(result.data_level.as_deref(), Some(level));
        assert!(result.checked_markets.is_empty());
    }
}

#[test]
fn complete_depth_strategy_preflight_rejects_tier_mismatch_and_bar_warmup() {
    let error = validate_strategy_execution_preflight_v1(
        &preflight(
            complete_depth_manifest("tick", 0),
            depth_strategy_spec("l2"),
        ),
        &research(),
    )
    .expect_err("Tick strategies must not run over OrderBook data");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(error.message().contains("requires depth tier=l1"));

    let error = validate_strategy_execution_preflight_v1(
        &preflight(
            complete_depth_manifest("orderbook", 4),
            depth_strategy_spec("l2"),
        ),
        &research(),
    )
    .expect_err("Bar-count warmup cannot be applied to snapshots");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(error.message().contains("只适用于 Bar"));
}

#[test]
fn complete_strategy_preflight_fails_closed_on_parameter_or_market_mismatch() {
    let root = scratch("strategy-preflight-mismatch");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 12);
    let mut execution = single_market_spec(&bars, 1);
    let error = validate_strategy_execution_preflight_v1(
        &preflight(complete_manifest(4), execution.clone()),
        &research(),
    )
    .expect_err("parameter below its declared minimum must be rejected");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);

    execution["parameters"]["lookback"] = json!(12);
    execution["market_spec"]["product"] = json!("future");
    let error = validate_strategy_execution_preflight_v1(
        &preflight(complete_manifest(4), execution),
        &research(),
    )
    .expect_err("market outside the declared product set must be rejected");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
}

#[test]
fn complete_strategy_preflight_rejects_warmup_without_a_decision_bar() {
    let root = scratch("strategy-preflight-warmup");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 6);
    let error = validate_strategy_execution_preflight_v1(
        &preflight(complete_manifest(6), single_market_spec(&bars, 12)),
        &research(),
    )
    .expect_err("warmup bars need one additional decision bar");
    assert_eq!(error.category(), AppErrorCategory::FidelityInsufficient);
}

#[test]
fn partial_strategy_manifest_preserves_legacy_behavior_without_claiming_checks() {
    let manifest = json!({
        "schema_version": 1,
        "kind": "qianxing-python-strategy-manifest",
        "strategy_id": "legacy-strategy",
        "strategy_version": "local",
        "entrypoint": "create_strategy(spec)",
        "parameters": {},
        "instrument_requirements": null,
        "data_level": null,
        "warmup_bars": null,
        "dependencies": [],
        "completeness": "partial"
    });
    let result = validate_strategy_execution_preflight_v1(
        &preflight(manifest, json!({"instrument": "BTCUSDT.BINANCE"})),
        &research(),
    )
    .expect("partial metadata retains compatibility");
    assert_eq!(result.completeness, "partial");
    assert!(result.checked_markets.is_empty());
}

#[test]
fn multi_market_preflight_checks_only_the_selected_strategy_market() {
    let root = scratch("strategy-preflight-selected-slot");
    let bars = write_frame(&root, "bars.json", "BTCUSDT.BINANCE", 12);
    let markets = json!({
        "markets": [
            {
                "instrument": "BTCUSDT.BINANCE",
                "bars_path": bars,
                "market_spec": {"product": "spot", "exchange_id": "BINANCE"},
                "asset_class": "crypto",
                "strategies": [{"strategy": {"strategy_id": "sample-sma", "lookback": 12}}]
            },
            {
                "instrument": "ETHUSDT.OKX",
                "bars_path": "must-not-be-opened.json",
                "market_spec": null,
                "asset_class": "crypto",
                "strategies": [{"strategy": {"strategy_id": "peer-strategy", "lookback": 12}}]
            }
        ]
    });
    let mut spec = preflight(complete_manifest(4), markets);
    spec.target_strategy_id = Some("sample-sma".into());
    let result = validate_strategy_execution_preflight_v1(&spec, &research())
        .expect("unselected market must not block the selected Python callback slot");
    assert_eq!(result.checked_markets.len(), 1);
    assert_eq!(result.checked_markets[0].label, "markets[0]");
}
