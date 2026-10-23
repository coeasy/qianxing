use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "multi_market_depth_quality_manifest.rs"]
mod multi_market_depth_quality_manifest;

fn root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .join("target")
        .join(format!("multi-market-quality-cli-{}", std::process::id()));
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
fn multi_market_quality_reports_survive_evidence_pack_and_offline_verification() {
    let root = root();
    let deploy = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let source: Value = serde_json::from_slice(
        &std::fs::read(deploy.join("qianxing.bar-frame.example.json")).unwrap(),
    )
    .unwrap();
    let instruments = ["BTCUSDT.BINANCE", "ETHUSDT.BINANCE", "SOLUSDT.BINANCE"];
    let mut markets = Vec::new();
    let mut quality_path = None;
    for (index, instrument) in instruments.iter().enumerate() {
        let frame_path = root.join(format!("market-{index}.json"));
        let mut frame = source.clone();
        frame["instrument"] = json!(instrument);
        std::fs::write(&frame_path, serde_json::to_vec(&frame).unwrap()).unwrap();
        if index == 0 {
            let manifest = root.join("market-0.quality.json");
            let output = cli(&[
                "data-validate",
                frame_path.to_str().unwrap(),
                "research:market-0",
                "daily-v1",
                "1000",
                manifest.to_str().unwrap(),
                "--timezone",
                "UTC",
            ]);
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            quality_path = Some(manifest);
        }
        let mut market = json!({
            "instrument": instrument,
            "bars_path": frame_path,
            "priority": index,
            "market_spec": null,
            "fee_schedule": null,
            "strategies": [{
                "priority": 0,
                "reserved_capital_raw": 20_000_000_000_000_i128,
                "strategy": {
                    "kind": "sma_cross",
                    "strategy_id": format!("multi-quality-{index}"),
                    "quantity_raw": 1_000_000_000_i128,
                    "fast_window": 1,
                    "slow_window": 3
                }
            }]
        });
        if index == 0 {
            market["dataset_manifest_path"] = json!(quality_path.as_ref().unwrap());
        }
        markets.push(market);
    }
    let artifact_root = root.join("runs");
    let experiment_markets = markets.clone();
    let spec = root.join("multi-market.json");
    std::fs::write(
        &spec,
        json!({
            "schema_version": 1,
            "run_id": "multi-market-quality-cli",
            "account_id": "multi-market-quality-account",
            "settlement_currency": "USDT",
            "initial_cash_raw": 100_000_000_000_000_i128,
            "seed": 7,
            "output_dir": artifact_root,
            "markets": markets
        })
        .to_string(),
    )
    .unwrap();
    let run = cli(&["app", "multi-market-backtest", spec.to_str().unwrap()]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let outcome: Value = serde_json::from_slice(&run.stdout).unwrap();
    let summary = PathBuf::from(outcome["artifact_path"].as_str().unwrap());
    let generated_diagnostics = root.join("multi-market-bar-diagnostics-generated.json");
    let diagnose = cli(&[
        "strategy",
        "diagnose-multi-bar",
        summary.to_str().unwrap(),
        "--output",
        generated_diagnostics.to_str().unwrap(),
        "--html",
    ]);
    assert!(
        diagnose.status.success(),
        "{}",
        String::from_utf8_lossy(&diagnose.stderr)
    );
    let generated: Value =
        serde_json::from_slice(&std::fs::read(&generated_diagnostics).unwrap()).unwrap();
    assert_eq!(generated.as_array().unwrap().len(), 3);
    assert!(generated.as_array().unwrap().iter().all(|item| {
        item["input_kind"] == "bar"
            && item["input_bars"].as_u64().is_some_and(|count| count >= 2)
            && item["lookahead"]["status"].as_str().is_some()
            && item["startup"]["trajectory"]["status"].as_str().is_some()
    }));
    let report_name = summary.file_name().unwrap().to_string_lossy();
    let generated_html = summary
        .with_file_name(report_name.replace(".multi-market.json", ".multi-market.report.html"));
    let generated_html = std::fs::read_to_string(generated_html).unwrap();
    assert!(generated_html.contains("strategyDiagnostics"));
    assert!(generated_html.contains("multi-quality-0"));
    let protected_bar = root.join("market-0.json");
    let protected_bytes = std::fs::read(&protected_bar).unwrap();
    let overwrite = cli(&[
        "report",
        summary.to_str().unwrap(),
        "--html",
        "--output",
        protected_bar.to_str().unwrap(),
    ]);
    assert!(!overwrite.status.success());
    assert_eq!(std::fs::read(&protected_bar).unwrap(), protected_bytes);
    let html = root.join("multi-market-report.html");
    let xlsx = root.join("multi-market-report.xlsx");
    let report = cli(&[
        "report",
        summary.to_str().unwrap(),
        "--json",
        "--html",
        "--output",
        html.to_str().unwrap(),
        "--xlsx",
        xlsx.to_str().unwrap(),
    ]);
    assert!(
        report.status.success(),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    let report_json: Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(report_json["report_kind"], "multi_market_bar");
    assert_eq!(
        report_json["report"]["markets"].as_array().unwrap().len(),
        3
    );
    assert_eq!(
        report_json["report"]["markets"][0]["quality_status"],
        "verified"
    );
    assert_eq!(
        report_json["report"]["instrument_equity_contributions"]["BTCUSDT.BINANCE"][0]
            ["equity_index"],
        0
    );
    assert!(report_json["report"]["events"]
        .as_array()
        .is_some_and(|events| !events.is_empty()));
    assert!(report_json["report"]["event_log_digest"].as_str().is_some());
    let rendered = std::fs::read_to_string(&html).unwrap();
    assert!(rendered.contains("权益曲线"));
    assert!(rendered.contains("按标的权益贡献"));
    assert!(rendered.contains("contributionInstrument"));
    assert!(rendered.contains("marketSearch"));
    assert!(rendered.contains("fillSearch"));
    assert!(rendered.contains("已验证事件时间线"));
    assert!(rendered.contains("eventSearch"));
    assert!(rendered.contains("eventLinks"));
    assert!(rendered.contains("onmousemove"));
    assert!(xlsx.is_file());

    let run_artifact: Value = serde_json::from_slice(&std::fs::read(&summary).unwrap()).unwrap();
    let diagnostics = run_artifact["spec"]["markets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|configured| {
            let instrument = configured["instrument"].as_str().unwrap();
            let market = run_artifact["market_data"]
                .as_array()
                .unwrap()
                .iter()
                .find(|market| market["instrument"].as_str() == Some(instrument))
                .unwrap();
            let frame: Value =
                serde_json::from_slice(&std::fs::read(market["path"].as_str().unwrap()).unwrap())
                    .unwrap();
            json!({
                "schema_version": 1,
                "kind": "qianxing-strategy-bias-diagnostics",
                "strategy_id": configured["strategies"][0]["strategy"]["strategy_id"],
                "instrument": instrument,
                "data_fingerprint": market["content_hash"],
                "input_kind": "bar",
                "input_bars": market["row_count"],
                "as_of": frame["ts"].as_array().unwrap().last().unwrap(),
                "lookahead": {"status": "no_change_detected", "checks": []},
                "startup": {"status": "stable_across_requested_windows", "divergences": []}
            })
        })
        .collect::<Vec<_>>();
    let diagnostics_path = root.join("multi-market-bar-diagnostics.json");
    std::fs::write(&diagnostics_path, serde_json::to_vec(&diagnostics).unwrap()).unwrap();
    let diagnostics_html = root.join("multi-market-with-diagnostics.html");
    let diagnostics_xlsx = root.join("multi-market-with-diagnostics.xlsx");
    let diagnostics_report = cli(&[
        "report",
        summary.to_str().unwrap(),
        "--strategy-diagnostics",
        diagnostics_path.to_str().unwrap(),
        "--json",
        "--html",
        "--output",
        diagnostics_html.to_str().unwrap(),
        "--xlsx",
        diagnostics_xlsx.to_str().unwrap(),
    ]);
    assert!(
        diagnostics_report.status.success(),
        "{}",
        String::from_utf8_lossy(&diagnostics_report.stderr)
    );
    let bound_report: Value = serde_json::from_slice(&diagnostics_report.stdout).unwrap();
    assert_eq!(
        bound_report["report"]["market_strategy_diagnostics"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let diagnostic_html = std::fs::read_to_string(&diagnostics_html).unwrap();
    assert!(diagnostic_html.contains("strategyDiagnostics"));
    assert!(diagnostic_html.contains("multi-quality-0"));
    assert!(diagnostics_xlsx.is_file());
    let mut diagnostics_workbook =
        zip::ZipArchive::new(std::fs::File::open(&diagnostics_xlsx).unwrap()).unwrap();
    let mut workbook_index = String::new();
    diagnostics_workbook
        .by_name("xl/workbook.xml")
        .unwrap()
        .read_to_string(&mut workbook_index)
        .unwrap();
    assert!(workbook_index.contains("StrategyDiagnostics"));
    assert!(workbook_index.contains("DiagnosticEvidence"));
    let mut evidence_sheet = String::new();
    diagnostics_workbook
        .by_name("xl/worksheets/sheet4.xml")
        .unwrap()
        .read_to_string(&mut evidence_sheet)
        .unwrap();
    assert!(evidence_sheet.contains("no_change_detected"));
    let mut invalid_diagnostics = diagnostics;
    invalid_diagnostics[0]["input_bars"] = json!(1);
    let invalid_diagnostics_path = root.join("multi-market-bar-diagnostics-invalid.json");
    std::fs::write(
        &invalid_diagnostics_path,
        serde_json::to_vec(&invalid_diagnostics).unwrap(),
    )
    .unwrap();
    let rejected_diagnostics = cli(&[
        "report",
        summary.to_str().unwrap(),
        "--strategy-diagnostics",
        invalid_diagnostics_path.to_str().unwrap(),
        "--json",
    ]);
    assert!(!rejected_diagnostics.status.success());
    assert!(String::from_utf8_lossy(&rejected_diagnostics.stderr).contains("Bar 数量"));

    let tampered_summary = summary.with_file_name("multi-market-tampered-events.json");
    let mut tampered: Value = serde_json::from_slice(&std::fs::read(&summary).unwrap()).unwrap();
    tampered["event_log"].as_array_mut().unwrap().pop();
    std::fs::write(&tampered_summary, serde_json::to_vec(&tampered).unwrap()).unwrap();
    let tampered_report = cli(&["report", tampered_summary.to_str().unwrap(), "--json"]);
    assert!(!tampered_report.status.success());
    assert!(String::from_utf8_lossy(&tampered_report.stderr).contains("事件流或账本"));

    let altered_fills_path = root.join("multi-market-altered-fills.json");
    let mut altered: Value = serde_json::from_slice(&std::fs::read(&summary).unwrap()).unwrap();
    let original_fills_path = altered["fills_path"].as_str().unwrap();
    let mut altered_fills: Value =
        serde_json::from_slice(&std::fs::read(original_fills_path).unwrap()).unwrap();
    assert!(!altered_fills.as_array().unwrap().is_empty());
    altered_fills[0]["price"] = json!(altered_fills[0]["price"].as_i64().unwrap() + 1);
    std::fs::write(
        &altered_fills_path,
        serde_json::to_vec(&altered_fills).unwrap(),
    )
    .unwrap();
    let altered_summary = summary.with_file_name("multi-market-altered-fill-summary.json");
    altered["fills_path"] = json!(altered_fills_path);
    std::fs::write(&altered_summary, serde_json::to_vec(&altered).unwrap()).unwrap();
    let altered_report = cli(&["report", altered_summary.to_str().unwrap(), "--json"]);
    assert!(!altered_report.status.success());
    assert!(String::from_utf8_lossy(&altered_report.stderr).contains("权益/成交产物与摘要不一致"));

    let second_spec = root.join("multi-market-second.json");
    let mut second: Value = serde_json::from_slice(&std::fs::read(&spec).unwrap()).unwrap();
    second["run_id"] = json!("multi-market-quality-cli-second");
    second["output_dir"] = json!(root.join("runs-second"));
    std::fs::write(&second_spec, second.to_string()).unwrap();
    let second_run = cli(&[
        "app",
        "multi-market-backtest",
        second_spec.to_str().unwrap(),
    ]);
    assert!(
        second_run.status.success(),
        "{}",
        String::from_utf8_lossy(&second_run.stderr)
    );
    let second_outcome: Value = serde_json::from_slice(&second_run.stdout).unwrap();
    let second_summary = second_outcome["artifact_path"].as_str().unwrap();
    let comparison_html = root.join("multi-market-comparison.html");
    let comparison = cli(&[
        "report",
        summary.to_str().unwrap(),
        "--against",
        second_summary,
        "--json",
        "--html",
        "--output",
        comparison_html.to_str().unwrap(),
    ]);
    assert!(
        comparison.status.success(),
        "{}",
        String::from_utf8_lossy(&comparison.stderr)
    );
    let comparison_json: Value = serde_json::from_slice(&comparison.stdout).unwrap();
    assert_eq!(
        comparison_json["report_kind"],
        "multi_market_bar_comparison"
    );
    assert_eq!(
        comparison_json["report"]["runs"].as_array().unwrap().len(),
        2
    );
    assert!(std::fs::read_to_string(comparison_html)
        .unwrap()
        .contains("同输入运行排名"));
    let package = cli(&[
        "evidence",
        "pack-multi-market-run",
        summary.to_str().unwrap(),
    ]);
    assert!(
        package.status.success(),
        "{}",
        String::from_utf8_lossy(&package.stderr)
    );
    let evidence_path = summary.with_file_name("multi-market-quality-cli.evidence.json");
    let evidence: Value = serde_json::from_slice(&std::fs::read(&evidence_path).unwrap()).unwrap();
    let component = evidence
        .pointer("/generation/run/dataset/components/0")
        .unwrap();
    assert!(component["manifest_fingerprint"].as_str().is_some());
    assert_eq!(component["quality_report"]["missing_intervals"], 0);
    assert!(component["quality_manifest_path"].as_str().is_some());
    assert!(
        evidence["generation"]["run"]["artifacts"]
            .as_array()
            .unwrap()
            .len()
            > 4
    );

    let verified = cli(&["evidence", "verify-run", evidence_path.to_str().unwrap()]);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );

    let run_artifact: Value = serde_json::from_slice(&std::fs::read(&summary).unwrap()).unwrap();
    let diagnostics: Vec<Value> = run_artifact["spec"]["markets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|market| {
            let instrument = market["instrument"].as_str().unwrap();
            let source = run_artifact["market_data"]
                .as_array()
                .unwrap()
                .iter()
                .find(|input| input["instrument"].as_str() == Some(instrument))
                .unwrap();
            let frame: Value =
                serde_json::from_slice(&std::fs::read(source["path"].as_str().unwrap()).unwrap())
                    .unwrap();
            let strategy_id = market["strategies"][0]["strategy"]["strategy_id"]
                .as_str()
                .unwrap();
            json!({
                "schema_version": 1,
                "kind": "qianxing-strategy-bias-diagnostics",
                "strategy_id": strategy_id,
                "instrument": instrument,
                "data_fingerprint": source["content_hash"],
                "as_of": frame["ts"].as_array().unwrap().last().unwrap(),
                "input_bars": source["row_count"],
                "lookahead": {"status":"no_change_detected"},
                "startup": {"status":"stable_across_requested_windows"}
            })
        })
        .collect();
    let diagnostics_path = root.join("multi-market-strategy-diagnostics.json");
    let diagnostics_bytes = serde_json::to_vec_pretty(&diagnostics).unwrap();
    std::fs::write(&diagnostics_path, &diagnostics_bytes).unwrap();
    let second_artifact: Value =
        serde_json::from_slice(&std::fs::read(second_summary).unwrap()).unwrap();
    let comparison_diagnostics = json!({
        "schema_version": 1,
        "kind": "qianxing-multi-market-bar-diagnostic-comparison",
        "runs": [
            {"run_id": run_artifact["spec"]["run_id"], "diagnostics": diagnostics},
            {"run_id": second_artifact["spec"]["run_id"], "diagnostics": diagnostics}
        ]
    });
    let comparison_diagnostics_path = root.join("multi-market-comparison-diagnostics.json");
    std::fs::write(
        &comparison_diagnostics_path,
        serde_json::to_vec_pretty(&comparison_diagnostics).unwrap(),
    )
    .unwrap();
    let diagnostic_comparison_html = root.join("multi-market-diagnostic-comparison.html");
    let diagnostic_comparison_xlsx = root.join("multi-market-diagnostic-comparison.xlsx");
    let diagnostic_comparison = cli(&[
        "report",
        summary.to_str().unwrap(),
        "--against",
        second_summary,
        "--strategy-diagnostics",
        comparison_diagnostics_path.to_str().unwrap(),
        "--json",
        "--html",
        "--output",
        diagnostic_comparison_html.to_str().unwrap(),
        "--xlsx",
        diagnostic_comparison_xlsx.to_str().unwrap(),
    ]);
    assert!(
        diagnostic_comparison.status.success(),
        "{}",
        String::from_utf8_lossy(&diagnostic_comparison.stderr)
    );
    let diagnostic_comparison_json: Value =
        serde_json::from_slice(&diagnostic_comparison.stdout).unwrap();
    let compared_runs = diagnostic_comparison_json["report"]["runs"]
        .as_array()
        .unwrap();
    assert_eq!(compared_runs.len(), 2);
    assert!(compared_runs
        .iter()
        .all(|run| run["market_strategy_diagnostics"]
            .as_array()
            .is_some_and(|items| items.len() == diagnostics.len())));
    let diagnostic_html = std::fs::read_to_string(diagnostic_comparison_html).unwrap();
    assert!(diagnostic_html.contains("逐运行策略诊断对照"));
    assert!(diagnostic_html.contains(run_artifact["spec"]["run_id"].as_str().unwrap()));
    assert!(diagnostic_html.contains(second_artifact["spec"]["run_id"].as_str().unwrap()));
    assert!(diagnostic_comparison_xlsx.is_file());

    let incomplete_diagnostics_path =
        root.join("multi-market-comparison-diagnostics-incomplete.json");
    let mut incomplete_diagnostics = comparison_diagnostics.clone();
    incomplete_diagnostics["runs"] = json!([comparison_diagnostics["runs"][0]]);
    std::fs::write(
        &incomplete_diagnostics_path,
        serde_json::to_vec_pretty(&incomplete_diagnostics).unwrap(),
    )
    .unwrap();
    let incomplete = cli(&[
        "report",
        summary.to_str().unwrap(),
        "--against",
        second_summary,
        "--strategy-diagnostics",
        incomplete_diagnostics_path.to_str().unwrap(),
        "--json",
    ]);
    assert!(!incomplete.status.success());
    assert!(String::from_utf8_lossy(&incomplete.stderr)
        .contains("run_id 集合必须与所有被比较运行完全一致"));
    let report_with_evidence = cli(&[
        "report",
        summary.to_str().unwrap(),
        "--json",
        "--evidence",
        "--strategy-diagnostics",
        diagnostics_path.to_str().unwrap(),
    ]);
    assert!(
        report_with_evidence.status.success(),
        "{}",
        String::from_utf8_lossy(&report_with_evidence.stderr)
    );
    let evidence_path =
        summary.with_file_name("multi-market-quality-cli.strategy-diagnostics.evidence.json");
    let evidence: Value = serde_json::from_slice(&std::fs::read(&evidence_path).unwrap()).unwrap();
    assert_eq!(
        evidence["generation"]["run"]["strategy_diagnostics"]["kind"],
        "strategy-bias-diagnostics-array"
    );
    let verified = cli(&["evidence", "verify-run", evidence_path.to_str().unwrap()]);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let verified_json: Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(verified_json["strategy_diagnostics_verified"], true);
    assert_eq!(verified_json["verified_attachment_count"], 1);

    let portable = root.join("portable-multi-market-run");
    let exported = cli(&[
        "evidence",
        "export-run",
        evidence_path.to_str().unwrap(),
        "--output-dir",
        portable.to_str().unwrap(),
    ]);
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let portable_bundle = portable.join("evidence.json");
    let portable_verified = cli(&["evidence", "verify-run", portable_bundle.to_str().unwrap()]);
    assert!(
        portable_verified.status.success(),
        "{}",
        String::from_utf8_lossy(&portable_verified.stderr)
    );
    let tampered = portable.join("inputs").join("strategy-diagnostics.json");
    let mut tampered_bytes = std::fs::read(&tampered).unwrap();
    tampered_bytes.push(b' ');
    std::fs::write(&tampered, tampered_bytes).unwrap();
    let rejected = cli(&["evidence", "verify-run", portable_bundle.to_str().unwrap()]);
    assert!(!rejected.status.success());
    let mut wrong_identity = diagnostics.clone();
    wrong_identity[0]["data_fingerprint"] = json!("different-market-data");
    let wrong_identity_bytes = serde_json::to_vec_pretty(&wrong_identity).unwrap();
    std::fs::write(&tampered, &wrong_identity_bytes).unwrap();
    let mut portable_evidence: Value =
        serde_json::from_slice(&std::fs::read(&portable_bundle).unwrap()).unwrap();
    portable_evidence["generation"]["run"]["strategy_diagnostics"]["input_sha256"] =
        json!(qx_strategy::sha256_hex(&wrong_identity_bytes));
    std::fs::write(
        &portable_bundle,
        serde_json::to_vec_pretty(&portable_evidence).unwrap(),
    )
    .unwrap();
    let rejected_identity = cli(&["evidence", "verify-run", portable_bundle.to_str().unwrap()]);
    assert!(!rejected_identity.status.success());
    assert!(String::from_utf8_lossy(&rejected_identity.stderr).contains("data_fingerprint"));

    let experiment_spec = root.join("multi-market-experiment.json");
    std::fs::write(
        &experiment_spec,
        json!({
            "schema_version": 1,
            "experiment_id": "multi-market-quality-grid",
            "target_strategy_id": "multi-quality-0",
            "base": {
                "schema_version": 1,
                "run_id": "unused-quality-grid-base",
                "account_id": "quality-grid-account",
                "settlement_currency": "USDT",
                "initial_cash_raw": 100_000_000_000_000_i128,
                "seed": 7,
                "output_dir": root.join("experiment-runs"),
                "markets": experiment_markets
            },
            "parameter_space": [{ "name": "fast_window", "values": [1] }]
        })
        .to_string(),
    )
    .unwrap();
    let experiment_run = cli(&[
        "app",
        "run-multi-market-experiment",
        experiment_spec.to_str().unwrap(),
    ]);
    assert!(
        experiment_run.status.success(),
        "{}",
        String::from_utf8_lossy(&experiment_run.stderr)
    );
    let experiment: Value = serde_json::from_slice(&experiment_run.stdout).unwrap();
    let experiment_artifact = experiment["artifact_path"].as_str().unwrap();
    let portable = root.join("portable-experiment");
    let packed = cli(&[
        "evidence",
        "pack-multi-market-experiment",
        experiment_artifact,
        "--output-dir",
        portable.to_str().unwrap(),
    ]);
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let portable_spec: Value =
        serde_json::from_slice(&std::fs::read(portable.join("experiment.spec.json")).unwrap())
            .unwrap();
    assert!(portable_spec
        .pointer("/base/markets/0/dataset_manifest_path")
        .and_then(Value::as_str)
        .is_some_and(|path| path.starts_with("inputs/market-01-quality.")));
    let bundle_verified = cli(&[
        "evidence",
        "verify-multi-market-experiment",
        portable.to_str().unwrap(),
        "--recompute",
    ]);
    assert!(
        bundle_verified.status.success(),
        "{}",
        String::from_utf8_lossy(&bundle_verified.stderr)
    );
}
