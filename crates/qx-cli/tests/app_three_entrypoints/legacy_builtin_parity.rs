use super::{repo_root, sample_bars, scratch, via_cli};
use serde_json::json;
use std::{path::Path, process::Command};

#[test]
fn legacy_builtin_strategies_match_qx_app_default_engine_results() {
    let root = scratch("legacy-builtin-parity");
    let bars = sample_bars();
    let strategies = [
        ("sma_cross", "0d4347c63a5254b3"),
        ("ema_cross", "269a4897dc0dd634"),
        ("macd", "1189853a7c12447d"),
        ("rsi", "b55b94c35e245949"),
        ("bollinger", "d9ec97b073b390c1"),
        ("donchian_breakout", "aa220624bbee6f30"),
        ("momentum", "bf35ae3aa96905de"),
        ("mean_reversion", "eabad7783bf905b2"),
        ("grid", "a2cf4a04a4bf9b21"),
        ("atr_trend", "e25fddfd488529b1"),
        ("keltner_trend", "acd3411959effe11"),
        ("vwap_reversion", "ffc60b4e616d42ae"),
        ("volatility_breakout", "e25fddfd488529b1"),
    ];
    for (kind, expected_hash) in strategies {
        let legacy_output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
            .args(["backtest", "builtin", kind, &bars])
            .output()
            .expect("start legacy builtin backtest");
        assert_eq!(
            legacy_output.status.code(),
            Some(0),
            "legacy {kind} backtest failed: {}",
            String::from_utf8_lossy(&legacy_output.stderr)
        );
        let legacy_stdout = String::from_utf8_lossy(&legacy_output.stdout);
        let legacy_metrics = legacy_stdout
            .lines()
            .find(|line| line.starts_with("[Strategy · Backtest]"))
            .unwrap_or_else(|| panic!("legacy {kind} metrics line missing"));
        let legacy_field = |name: &str| {
            legacy_metrics
                .split_whitespace()
                .find_map(|field| field.strip_prefix(&format!("{name}=")))
                .unwrap_or_else(|| panic!("legacy {kind} metrics omit {name}: {legacy_metrics}"))
        };

        let run_id = "strategy-backtest-strategy-builtin-BTCUSDT.BINANCE";
        let spec = json!({
            "schema_version": 1,
            "run_id": run_id,
            "instrument": "BTCUSDT.BINANCE",
            "bars_path": bars,
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000_i128,
            "seed": 20260911,
            "output_dir": root.join(format!("app-artifacts-{kind}")),
            "account_id": "main",
            "strategy": {
                "kind": kind,
                "strategy_id": "strategy-builtin",
                "quantity_raw": 1_000_000_000_i128,
                "fast_window": 5,
                "slow_window": 20,
                "period": 14,
                "threshold_bps": 100,
            },
        })
        .to_string();
        let app = via_cli(&spec, &root);
        assert_eq!(
            app["result_hash"],
            legacy_field("result_hash"),
            "{kind}: CLI stdout={legacy_stdout}; app outcome={app}"
        );
        assert_eq!(
            legacy_field("result_hash"),
            expected_hash,
            "{kind} baseline"
        );
        assert_eq!(
            app["fills"].as_u64().unwrap().to_string(),
            legacy_field("fills"),
            "{kind}"
        );
        assert_eq!(
            app["return_bps"].as_i64().unwrap().to_string(),
            legacy_field("return_bps"),
            "{kind}"
        );
        assert_eq!(
            app["max_drawdown_bps"].as_u64().unwrap().to_string(),
            legacy_field("max_drawdown_bps"),
            "{kind}"
        );
        for key in ["run_manifest", "summary", "equity", "fills"] {
            let path = app["artifacts"][key].as_str().expect("artifact path");
            assert!(
                Path::new(path).is_file(),
                "missing {kind} app artifact: {path}"
            );
        }
    }
}

#[test]
fn configured_builtin_matches_the_qx_app_spec_after_signal_and_fill_binding() {
    let root = scratch("configured-builtin-parity");
    let bars = sample_bars();
    let mut runtime: serde_json::Value = serde_json::from_slice(
        &std::fs::read(repo_root().join("deploy/qianxing.runtime.example.json"))
            .expect("read runtime template"),
    )
    .expect("parse runtime template");
    runtime["strategy"]["builtin_fast_window"] = json!(2);
    runtime["strategy"]["builtin_slow_window"] = json!(3);
    runtime["strategy"]["fill_model"] = json!("best_price");
    let runtime_path = root.join("runtime.json");
    std::fs::write(
        &runtime_path,
        serde_json::to_vec_pretty(&runtime).expect("serialize configured runtime"),
    )
    .expect("write configured runtime");

    let legacy = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(["backtest", "builtin", "sma_cross", &bars, "--config"])
        .arg(&runtime_path)
        .output()
        .expect("start configured legacy backtest");
    assert_eq!(
        legacy.status.code(),
        Some(0),
        "configured legacy backtest failed: {}",
        String::from_utf8_lossy(&legacy.stderr)
    );
    let stdout = String::from_utf8_lossy(&legacy.stdout);
    let metrics = stdout
        .lines()
        .find(|line| line.starts_with("[Strategy · Backtest]"))
        .expect("configured legacy metrics line");
    let field = |name: &str| {
        metrics
            .split_whitespace()
            .find_map(|value| value.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("configured metrics omit {name}: {metrics}"))
    };
    assert!(stdout.contains("[Strategy · Signal] source=config fast_window=2 slow_window=3"));
    let summary_path = stdout
        .lines()
        .find(|line| line.starts_with("[Artifacts] "))
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|field| field.strip_prefix("summary="))
        })
        .expect("configured backtest summary path");
    let summary: serde_json::Value = serde_json::from_slice(
        &std::fs::read(summary_path).expect("read configured backtest summary"),
    )
    .expect("parse configured backtest summary");
    assert_eq!(summary["fill_model"]["name"], "best_price");

    let spec = json!({
        "schema_version": 1,
        "run_id": "configured-builtin-parity",
        "instrument": "BTCUSDT.BINANCE",
        "bars_path": bars,
        "settlement_currency": "USDT",
        "initial_cash_raw": 100_000_000_000_000_i128,
        "seed": 20260911,
        "output_dir": root.join("app-artifacts"),
        "account_id": "main",
        "fill_model": "best_price",
        "strategy": {
            "kind": "sma_cross",
            "strategy_id": "builtin-sma_cross",
            "quantity_raw": 1_000_000_000_i128,
            "fast_window": 2,
            "slow_window": 3,
            "period": 14,
            "threshold_bps": 100,
        },
    })
    .to_string();
    let app = via_cli(&spec, &root);
    assert_eq!(app["result_hash"], field("result_hash"));
    assert_eq!(app["fills"].as_u64().unwrap().to_string(), field("fills"));
    assert_eq!(
        app["return_bps"].as_i64().unwrap().to_string(),
        field("return_bps")
    );
    assert_eq!(
        app["max_drawdown_bps"].as_u64().unwrap().to_string(),
        field("max_drawdown_bps")
    );
}
