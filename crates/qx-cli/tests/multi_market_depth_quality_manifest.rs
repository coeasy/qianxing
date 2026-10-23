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
        .join(format!(
            "multi-market-depth-quality-{}-{}",
            std::process::id(),
            module_path!().replace("::", "-")
        ));
    if root.exists() {
        std::fs::remove_dir_all(&root).expect("remove stale multi-market depth quality fixture");
    }
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

    let diagnostic_path = root.join("multi-market-depth-diagnostics.json");
    let diagnostic_path_text = diagnostic_path.to_string_lossy().into_owned();
    let generated = cli(&[
        "strategy",
        "diagnose-depth",
        &summary_text,
        "--output",
        &diagnostic_path_text,
        "--html",
    ]);
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let diagnostics: Value =
        serde_json::from_slice(&std::fs::read(&diagnostic_path).unwrap()).unwrap();
    assert_eq!(diagnostics.as_array().unwrap().len(), 2);
    assert_eq!(diagnostics[0]["lookahead"]["status"], "no_change_detected");
    assert!(
        diagnostics[0]["lookahead"]["perturbation_targets_checked"]
            .as_u64()
            .unwrap_or_default()
            > 0
    );
    assert!(
        diagnostics[0]["lookahead"]["decision_points_checked"]
            .as_u64()
            .unwrap_or_default()
            > 0
    );
    assert_eq!(
        diagnostics[0]["data_visibility"]["status"],
        "visibility_unknown"
    );
    assert_eq!(
        diagnostics[0]["startup"]["windows"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(diagnostics[0]["input_kind"], "depth_midpoint_bar");
    let report_html_path =
        summary.with_file_name(summary.file_name().unwrap().to_string_lossy().replace(
            ".multi-market-depth.json",
            ".multi-market-depth.report.html",
        ));
    let generated_html = std::fs::read_to_string(&report_html_path).unwrap();
    assert!(generated_html.contains("no_change_detected"));
    assert!(generated_html.contains("visibility_unknown"));
    let attached_xlsx = root.join("multi-market-depth-diagnostics.xlsx");
    let attached_xlsx_text = attached_xlsx.to_string_lossy().into_owned();
    let attached = cli(&[
        "report",
        &summary_text,
        "--strategy-diagnostics",
        &diagnostic_path_text,
        "--json",
        "--html",
        "--xlsx",
        &attached_xlsx_text,
    ]);
    assert!(
        attached.status.success(),
        "{}",
        String::from_utf8_lossy(&attached.stderr)
    );
    let attached_json: Value = serde_json::from_slice(&attached.stdout).unwrap();
    assert_eq!(
        attached_json["report"]["market_strategy_diagnostics"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let attached_html = std::fs::read_to_string(
        attached_json["generated_artifacts"]["html"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert!(attached_html.contains("diagnostic-filter"));
    assert!(attached_html.contains("depth-quality-0"));
    assert_eq!(
        attached_json["generated_artifacts"]["xlsx"],
        attached_xlsx_text
    );
    let mut attached_workbook =
        zip::ZipArchive::new(std::fs::File::open(&attached_xlsx).unwrap()).unwrap();
    let mut workbook_index = String::new();
    attached_workbook
        .by_name("xl/workbook.xml")
        .unwrap()
        .read_to_string(&mut workbook_index)
        .unwrap();
    assert!(workbook_index.contains("StrategyDiagnostics"));
    assert!(workbook_index.contains("DiagnosticEvidence"));
    let mut diagnostic_sheet = String::new();
    attached_workbook
        .by_name("xl/worksheets/sheet3.xml")
        .unwrap()
        .read_to_string(&mut diagnostic_sheet)
        .unwrap();
    assert!(diagnostic_sheet.contains("no_change_detected"));
    let mut evidence_sheet = String::new();
    attached_workbook
        .by_name("xl/worksheets/sheet4.xml")
        .unwrap()
        .read_to_string(&mut evidence_sheet)
        .unwrap();
    assert!(evidence_sheet.contains("input_kind"));
    assert!(evidence_sheet.contains("depth_midpoint_bar"));

    let evidence_report = cli(&[
        "report",
        &summary_text,
        "--strategy-diagnostics",
        &diagnostic_path_text,
        "--json",
        "--evidence",
    ]);
    assert!(
        evidence_report.status.success(),
        "{}",
        String::from_utf8_lossy(&evidence_report.stderr)
    );
    let evidence_report: Value = serde_json::from_slice(&evidence_report.stdout).unwrap();
    let diagnostic_evidence = PathBuf::from(
        evidence_report["generated_artifacts"]["evidence"]
            .as_str()
            .unwrap(),
    );
    let diagnostic_bundle: Value =
        serde_json::from_slice(&std::fs::read(&diagnostic_evidence).unwrap()).unwrap();
    let diagnostic_attachment = &diagnostic_bundle["generation"]["run"]["strategy_diagnostics"];
    assert_eq!(
        diagnostic_attachment["kind"],
        "strategy-bias-diagnostics-depth-array"
    );
    assert_eq!(
        diagnostic_attachment["input_sha256"],
        qx_strategy::sha256_hex(&std::fs::read(&diagnostic_path).unwrap())
    );
    let evidence_text = diagnostic_evidence.to_string_lossy().into_owned();
    let verified_diagnostic_bundle =
        cli(&["evidence", "verify-run", &evidence_text, "--recompute"]);
    assert!(
        verified_diagnostic_bundle.status.success(),
        "{}",
        String::from_utf8_lossy(&verified_diagnostic_bundle.stderr)
    );
    let portable_dir = root.join("portable-depth-diagnostics");
    let portable_export = cli(&[
        "evidence",
        "export-run",
        &evidence_text,
        "--output-dir",
        portable_dir.to_str().unwrap(),
    ]);
    assert!(
        portable_export.status.success(),
        "{}",
        String::from_utf8_lossy(&portable_export.stderr)
    );
    let portable_bundle = portable_dir.join("evidence.json");
    let portable_text = portable_bundle.to_string_lossy().into_owned();
    let portable_verified = cli(&["evidence", "verify-run", &portable_text]);
    assert!(
        portable_verified.status.success(),
        "{}",
        String::from_utf8_lossy(&portable_verified.stderr)
    );
    let portable_diagnostics = portable_dir.join("inputs/strategy-diagnostics.json");
    let mut altered_diagnostics: Value =
        serde_json::from_slice(&std::fs::read(&portable_diagnostics).unwrap()).unwrap();
    altered_diagnostics[0]["input_events"] = json!(1);
    let altered_bytes = serde_json::to_vec(&altered_diagnostics).unwrap();
    std::fs::write(&portable_diagnostics, &altered_bytes).unwrap();
    let mut altered_bundle: Value =
        serde_json::from_slice(&std::fs::read(&portable_bundle).unwrap()).unwrap();
    altered_bundle["generation"]["run"]["strategy_diagnostics"]["input_sha256"] =
        json!(qx_strategy::sha256_hex(&altered_bytes));
    std::fs::write(
        &portable_bundle,
        serde_json::to_vec_pretty(&altered_bundle).unwrap(),
    )
    .unwrap();
    let rejected_identity = cli(&["evidence", "verify-run", &portable_text]);
    assert!(!rejected_identity.status.success());
    assert!(String::from_utf8_lossy(&rejected_identity.stderr).contains("事件数"));

    let mut invalid_diagnostics = diagnostics.clone();
    invalid_diagnostics[0]["input_events"] = json!(1);
    let invalid_path = root.join("multi-market-depth-diagnostics-invalid.json");
    std::fs::write(
        &invalid_path,
        serde_json::to_vec(&invalid_diagnostics).unwrap(),
    )
    .unwrap();
    let invalid_path_text = invalid_path.to_string_lossy().into_owned();
    let rejected = cli(&[
        "report",
        &summary_text,
        "--strategy-diagnostics",
        &invalid_path_text,
        "--json",
    ]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("事件数"));

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

    let mut second_diagnostics = diagnostics.clone();
    for diagnostic in second_diagnostics.as_array_mut().unwrap() {
        let instrument = diagnostic["instrument"].as_str().unwrap();
        let strategy_id = second_spec["markets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|market| market["instrument"].as_str() == Some(instrument))
            .unwrap()["strategy"]["strategy_id"]
            .as_str()
            .unwrap();
        diagnostic["strategy_id"] = json!(strategy_id);
    }
    let comparison_diagnostics_path = root.join("depth-comparison-diagnostics.json");
    let comparison_diagnostics = json!({
        "schema_version":1,
        "kind":"qianxing-multi-market-depth-diagnostic-comparison",
        "runs":[
            {"run_id":"multi-market-depth-quality","diagnostics":diagnostics},
            {"run_id":"multi-market-depth-quality-alt","diagnostics":second_diagnostics}
        ]
    });
    std::fs::write(
        &comparison_diagnostics_path,
        serde_json::to_vec(&comparison_diagnostics).unwrap(),
    )
    .unwrap();
    let comparison_diagnostics_path_text =
        comparison_diagnostics_path.to_string_lossy().into_owned();
    let comparison_diagnostics_html = root.join("depth-comparison-diagnostics.html");
    let comparison_diagnostics_html_text =
        comparison_diagnostics_html.to_string_lossy().into_owned();
    let comparison_diagnostics_xlsx = root.join("depth-comparison-diagnostics.xlsx");
    let comparison_diagnostics_xlsx_text =
        comparison_diagnostics_xlsx.to_string_lossy().into_owned();
    let comparison_with_diagnostics = cli(&[
        "report",
        &summary_text,
        "--against",
        second_summary,
        "--strategy-diagnostics",
        &comparison_diagnostics_path_text,
        "--json",
        "--html",
        "--xlsx",
        &comparison_diagnostics_xlsx_text,
        "--output",
        &comparison_diagnostics_html_text,
    ]);
    assert!(
        comparison_with_diagnostics.status.success(),
        "{}",
        String::from_utf8_lossy(&comparison_with_diagnostics.stderr)
    );
    let comparison_with_diagnostics_json: Value =
        serde_json::from_slice(&comparison_with_diagnostics.stdout).unwrap();
    let comparison_runs = comparison_with_diagnostics_json["report"]["runs"]
        .as_array()
        .unwrap();
    assert_eq!(comparison_runs.len(), 2);
    assert!(comparison_runs.iter().all(|run| {
        run["market_strategy_diagnostics"]
            .as_array()
            .is_some_and(|items| {
                !items.is_empty() && items.iter().all(|item| item["identity_bound"] == true)
            })
    }));
    let comparison_html = std::fs::read_to_string(&comparison_diagnostics_html).unwrap();
    assert!(comparison_html.contains("comparison-diagnostic-filter"));
    assert!(comparison_html.contains("multi-market-depth-quality-alt"));
    let mut diagnostic_workbook =
        zip::ZipArchive::new(std::fs::File::open(&comparison_diagnostics_xlsx).unwrap()).unwrap();
    let mut diagnostic_summary_sheet = String::new();
    diagnostic_workbook
        .by_name("xl/worksheets/sheet3.xml")
        .unwrap()
        .read_to_string(&mut diagnostic_summary_sheet)
        .unwrap();
    assert!(diagnostic_summary_sheet.contains("run_id"));
    assert!(diagnostic_summary_sheet.contains("multi-market-depth-quality-alt"));

    let mut incomplete_comparison_diagnostics = comparison_diagnostics.clone();
    incomplete_comparison_diagnostics["runs"] =
        json!([incomplete_comparison_diagnostics["runs"][0].clone()]);
    let incomplete_diagnostics_path = root.join("depth-comparison-diagnostics-incomplete.json");
    std::fs::write(
        &incomplete_diagnostics_path,
        serde_json::to_vec(&incomplete_comparison_diagnostics).unwrap(),
    )
    .unwrap();
    let incomplete_diagnostics_path_text =
        incomplete_diagnostics_path.to_string_lossy().into_owned();
    let rejected_incomplete_comparison = cli(&[
        "report",
        &summary_text,
        "--against",
        second_summary,
        "--strategy-diagnostics",
        &incomplete_diagnostics_path_text,
        "--json",
    ]);
    assert!(!rejected_incomplete_comparison.status.success());
    assert!(
        String::from_utf8_lossy(&rejected_incomplete_comparison.stderr).contains("run_id 集合")
    );

    let evidence_text = evidence.to_string_lossy().into_owned();
    let verified = cli(&["evidence", "verify-run", &evidence_text, "--recompute"]);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
}
