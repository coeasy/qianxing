use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("target")
        .join(format!("multi-market-depth-quality-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn cli(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(arguments)
        .output()
        .expect("start qx-cli")
}

#[test]
fn multi_market_depth_quality_survives_evidence_pack_and_offline_recompute() {
    let root = root();
    let source: Value = serde_json::from_slice(
        &std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../deploy/qianxing.depth-frame.example.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let instruments = [
        ("BTCUSDT.BINANCE", "BTC", "BINANCE"),
        ("ETHUSDT.BYBIT", "ETH", "BYBIT"),
    ];
    let mut markets = Vec::new();
    for (index, (instrument, base, venue)) in instruments.iter().enumerate() {
        let (symbol, _) = instrument.split_once('.').unwrap();
        let path = root.join(format!("depth-{index}.json"));
        let mut frame = source.clone();
        frame["instrument"] = json!({"symbol":symbol,"venue":venue});
        for snapshot in frame["snapshots"].as_array_mut().unwrap() {
            snapshot["instrument"] = json!({"symbol":symbol,"venue":venue});
        }
        std::fs::write(&path, serde_json::to_vec(&frame).unwrap()).unwrap();
        let quality = root.join(format!("depth-{index}.quality.json"));
        let path_text = path.to_string_lossy().into_owned();
        let quality_text = quality.to_string_lossy().into_owned();
        let validated = cli(&[
            "data-validate-depth",
            &path_text,
            "--tier",
            "l2",
            "1000",
            &quality_text,
        ]);
        assert!(
            validated.status.success(),
            "{}",
            String::from_utf8_lossy(&validated.stderr)
        );
        markets.push(json!({
            "instrument": instrument,
            "depth_path": path,
            "dataset_manifest_path": quality,
            "data_tier": "l2",
            "priority": index,
            "market_spec": {
                "instrument": {"symbol":symbol,"venue":venue},
                "product":"spot", "base_currency":base, "quote_currency":"USDT", "settlement_currency":"USDT",
                "contract_size":1_000_000_000_i128,"linear":false,"inverse":false,"price_tick":1_i128,
                "qty_step":1_i128,"min_qty":1_i128,"max_leverage":1,"maintenance_margin_bps":0,
                "valid_from":1_u64,"valid_to":null
            },
            "fee_bps":0,"latency_snapshots":0,"queue_position_bps":0,"market_impact_bps":0,
            "strategy":{"kind":"sma_cross","strategy_id":format!("depth-quality-{index}"),"quantity_raw":1_000_000_000_i128,"fast_window":2,"slow_window":3}
        }));
    }
    let spec = root.join("multi-market-depth.json");
    std::fs::write(&spec, json!({
        "schema_version":1,"run_id":"multi-market-depth-quality","account_id":"shared-depth-account",
        "settlement_currency":"USDT","initial_cash_raw":100_000_000_000_000_i128,
        "output_dir":root.join("runs"),"markets":markets
    }).to_string()).unwrap();
    let spec_text = spec.to_string_lossy().into_owned();
    let run = cli(&["app", "multi-market-depth-backtest", &spec_text]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let outcome: Value = serde_json::from_slice(&run.stdout).unwrap();
    let summary = PathBuf::from(outcome["artifact_path"].as_str().unwrap());
    let summary_text = summary.to_string_lossy().into_owned();
    let packed = cli(&["evidence", "pack-multi-market-run", &summary_text]);
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let evidence = summary.with_file_name("multi-market-depth-quality.evidence.json");
    let json: Value = serde_json::from_slice(&std::fs::read(&evidence).unwrap()).unwrap();
    let component = &json["generation"]["run"]["dataset"]["components"][0];
    assert!(component["manifest_fingerprint"].as_str().is_some());
    assert_eq!(component["quality_report"]["interval_ms"], 1000);
    assert_eq!(
        json["generation"]["run"]["artifacts"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    let xlsx = root.join("multi-market-depth-quality.xlsx");
    let xlsx_text = xlsx.to_string_lossy().into_owned();
    let report = cli(&[
        "report",
        &summary_text,
        "--json",
        "--html",
        "--xlsx",
        &xlsx_text,
    ]);
    assert!(
        report.status.success(),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    let report_json: Value = serde_json::from_slice(&report.stdout).unwrap();
    let html_path = PathBuf::from(report_json["generated_artifacts"]["html"].as_str().unwrap());
    let html = std::fs::read_to_string(html_path).unwrap();
    assert!(html.contains("质量清单指纹"));
    assert!(html.contains(component["manifest_fingerprint"].as_str().unwrap()));
    assert!(xlsx.is_file() && std::fs::metadata(&xlsx).unwrap().len() > 0);
    let file = std::fs::File::open(&xlsx).unwrap();
    let mut workbook = zip::ZipArchive::new(file).unwrap();
    let mut quality_sheet = String::new();
    workbook
        .by_name("xl/worksheets/sheet2.xml")
        .unwrap()
        .read_to_string(&mut quality_sheet)
        .unwrap();
    assert!(quality_sheet.contains("manifest_fingerprint"));
    assert!(quality_sheet.contains(component["manifest_fingerprint"].as_str().unwrap()));
    let mut second_spec: Value = serde_json::from_slice(&std::fs::read(&spec).unwrap()).unwrap();
    second_spec["run_id"] = json!("multi-market-depth-quality-alt");
    second_spec["output_dir"] = json!(root.join("runs-alt"));
    for market in second_spec["markets"].as_array_mut().unwrap() {
        let old = market["strategy"]["strategy_id"]
            .as_str()
            .unwrap()
            .to_string();
        market["strategy"]["strategy_id"] = json!(format!("{old}-alt"));
        market["strategy"]["fast_window"] = json!(3);
        market["strategy"]["slow_window"] = json!(7);
    }
    let second_spec_path = root.join("multi-market-depth-alt.json");
    std::fs::write(&second_spec_path, second_spec.to_string()).unwrap();
    let second_spec_text = second_spec_path.to_string_lossy().into_owned();
    let second_run = cli(&["app", "multi-market-depth-backtest", &second_spec_text]);
    assert!(
        second_run.status.success(),
        "{}",
        String::from_utf8_lossy(&second_run.stderr)
    );
    let second_outcome: Value = serde_json::from_slice(&second_run.stdout).unwrap();
    let second_summary = second_outcome["artifact_path"].as_str().unwrap();
    let comparison_xlsx = root.join("depth-comparison.xlsx");
    let comparison_xlsx_text = comparison_xlsx.to_string_lossy().into_owned();
    let comparison = cli(&[
        "report",
        &summary_text,
        "--against",
        second_summary,
        "--json",
        "--html",
        "--xlsx",
        &comparison_xlsx_text,
    ]);
    assert!(
        comparison.status.success(),
        "{}",
        String::from_utf8_lossy(&comparison.stderr)
    );
    let comparison_json: Value = serde_json::from_slice(&comparison.stdout).unwrap();
    assert_eq!(
        comparison_json["report_kind"],
        "multi_market_depth_comparison"
    );
    assert_eq!(
        comparison_json["report"]["ranking"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(comparison_json["generated_artifacts"]["html"]
        .as_str()
        .is_some());
    assert!(comparison_xlsx.is_file());
    let mut comparison_workbook =
        zip::ZipArchive::new(std::fs::File::open(&comparison_xlsx).unwrap()).unwrap();
    let mut ranking_sheet = String::new();
    comparison_workbook
        .by_name("xl/worksheets/sheet2.xml")
        .unwrap()
        .read_to_string(&mut ranking_sheet)
        .unwrap();
    assert!(ranking_sheet.contains("multi-market-depth-quality"));
    assert!(ranking_sheet.contains("multi-market-depth-quality-alt"));
    let evidence_text = evidence.to_string_lossy().into_owned();
    let verified = cli(&["evidence", "verify-run", &evidence_text, "--recompute"]);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
}
