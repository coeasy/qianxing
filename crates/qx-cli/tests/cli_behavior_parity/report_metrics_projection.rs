//! One real `report` invocation projects a verified run to JSON, HTML, XLSX, and benchmark CSV.

use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn scratch() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli test workspace root")
        .join("target")
        .join(format!(
            "qx-cli-report-metrics-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(&root).expect("create report fixture");
    root
}

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(root)
        .env("TEMP", root)
        .env("TMP", root)
        .env_remove("QX_DEPLOY_DIR")
        .env_remove("QX_PYTHON")
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run qx-cli {args:?}: {error}"))
}

fn successful(root: &Path, args: &[&str]) -> Output {
    let output = run(root, args);
    assert!(
        output.status.success(),
        "qx-cli {args:?} failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn report_cli_projects_one_verified_benchmark_analysis_to_json_html_xlsx_and_csv() {
    let root = scratch();
    let output_dir = root.join("backtest-artifacts");
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli test workspace root");
    let bars_path = workspace
        .join("deploy/qianxing.bar-frame.example.json")
        .to_string_lossy()
        .into_owned();
    let spec_path = root.join("backtest-spec.json");
    let spec = json!({
        "schema_version": 1,
        "run_id": "r17-report-projection",
        "instrument": "BTCUSDT.BINANCE",
        "bars_path": bars_path,
        "settlement_currency": "USDT",
        "initial_cash_raw": 100_000_000_000_000_i128,
        "seed": 20261030,
        "output_dir": output_dir.to_string_lossy(),
        "strategy": {
            "kind": "sma_cross",
            "strategy_id": "r17-sma",
            "quantity_raw": 1_000_000_000_i128,
            "fast_window": 2,
            "slow_window": 3,
            "target_volatility_bps": 25
        }
    });
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
    let spec_text = spec_path.to_string_lossy().into_owned();
    successful(&root, &["app", "backtest", &spec_text]);

    let summary_path = output_dir.join("r17-report-projection.summary.json");
    let summary: Value = serde_json::from_slice(
        &std::fs::read(&summary_path).expect("app backtest writes durable summary"),
    )
    .expect("verified RunSummary JSON");
    assert_eq!(summary["schema_version"], 1);
    assert_eq!(summary["metrics"]["return_bps"], summary["return_bps"]);
    assert_eq!(
        summary["metrics"]["max_drawdown_bps"],
        summary["max_drawdown_bps"]
    );
    assert_eq!(summary["metrics_definition"]["schema_version"], 1);
    assert!(summary["run_record"].is_string());
    assert!(summary["run_manifest"].is_string());

    let equity_path = PathBuf::from(
        summary["artifact_locations"]["equity"]
            .as_str()
            .expect("equity artifact locator"),
    );
    let equity = std::fs::read_to_string(&equity_path).expect("read generated equity facts");
    let mut equity_lines = equity.lines();
    let timestamp_column = equity_lines
        .next()
        .and_then(|header| header.split(',').position(|column| column == "ts"))
        .expect("equity projection exposes the canonical timestamp column");
    let timestamps = equity_lines
        .filter_map(|line| line.split(',').nth(timestamp_column)?.parse::<u64>().ok())
        .collect::<Vec<_>>();
    assert!(
        timestamps.len() >= 3,
        "benchmark needs at least three samples"
    );

    // A deterministic synthetic benchmark isolates report projection behavior from data
    // licensing and venue access. Its timestamps exactly match the verified portfolio run.
    let benchmark_path = root.join("benchmark-bars.json");
    let close = (0..timestamps.len())
        .map(|index| 4_000_000_000_i64 + index as i64 * 10_000_000)
        .collect::<Vec<_>>();
    std::fs::write(
        &benchmark_path,
        serde_json::to_vec(&json!({
            "instrument": "000300.SSE",
            "source": "report-cli-synthetic-benchmark-v1",
            "ts": timestamps,
            "open_raw": close,
            "high_raw": close.iter().map(|value| value + 1_000_000).collect::<Vec<_>>(),
            "low_raw": close.iter().map(|value| value - 1_000_000).collect::<Vec<_>>(),
            "close_raw": close,
            "volume_raw": vec![1_000_000_i64; timestamps.len()]
        }))
        .unwrap(),
    )
    .unwrap();

    let html = root.join("daily-report.html");
    let xlsx = root.join("daily-report.xlsx");
    let csv = root.join("daily-benchmark.csv");
    let summary_text = summary_path.to_string_lossy().into_owned();
    let benchmark_text = benchmark_path.to_string_lossy().into_owned();
    let html_text_path = html.to_string_lossy().into_owned();
    let xlsx_text_path = xlsx.to_string_lossy().into_owned();
    let csv_text_path = csv.to_string_lossy().into_owned();
    let report_output = successful(
        &root,
        &[
            "report",
            &summary_text,
            "--json",
            "--html",
            "--benchmark-bars",
            &benchmark_text,
            "--benchmark-csv",
            &csv_text_path,
            "--xlsx",
            &xlsx_text_path,
            "--output",
            &html_text_path,
        ],
    );
    let report: Value = serde_json::from_slice(&report_output.stdout).expect("report JSON output");
    let analysis = &report["benchmark_analysis"];
    let series = analysis["series"]
        .as_array()
        .expect("canonical benchmark series");
    assert_eq!(series.len(), timestamps.len() - 1);
    assert_eq!(analysis["benchmark"]["instrument"], "000300.SSE");
    assert_eq!(analysis["period_basis"], "per_bar_no_annualization");
    assert_eq!(report["input_check"]["verdict"], "verified");
    assert_eq!(report["verification"]["verified"], true);
    assert_eq!(
        report["summary"]["metrics"]["return_bps"],
        summary["return_bps"]
    );
    assert_eq!(report["summary"]["metrics_definition"]["schema_version"], 1);
    assert_eq!(report["generated_artifacts"]["html"], html_text_path);
    assert_eq!(report["generated_artifacts"]["xlsx"], xlsx_text_path);
    assert_eq!(
        report["generated_artifacts"]["benchmark_csv"],
        csv_text_path
    );

    let csv_content = std::fs::read_to_string(&csv).expect("benchmark CSV generated");
    let csv_lines = csv_content.lines().collect::<Vec<_>>();
    assert_eq!(csv_lines.len(), series.len() + 1);
    for (line, period) in csv_lines.iter().skip(1).zip(series) {
        let fields = line.split(',').collect::<Vec<_>>();
        assert_eq!(fields[4], "per_bar_no_annualization");
        for (column, key) in [
            (5, "start_ts"),
            (6, "end_ts"),
            (7, "portfolio_equity_start_raw"),
            (8, "portfolio_equity_end_raw"),
            (9, "benchmark_close_start_raw"),
            (10, "benchmark_close_end_raw"),
            (11, "portfolio_return_bps"),
            (12, "benchmark_return_bps"),
            (13, "active_return_bps"),
        ] {
            assert_eq!(fields[column], period[key].to_string(), "CSV {key}");
        }
    }

    let html_content = std::fs::read_to_string(&html).expect("HTML generated");
    assert!(html_content.contains("基准对照：000300.SSE"));
    assert!(html_content.contains("信息比率(每 Bar)"));
    assert!(html_content.contains("指标口径 v1"));

    let mut workbook = zip::ZipArchive::new(std::fs::File::open(&xlsx).unwrap()).unwrap();
    let mut workbook_xml = String::new();
    workbook
        .by_name("xl/workbook.xml")
        .unwrap()
        .read_to_string(&mut workbook_xml)
        .unwrap();
    assert!(workbook_xml.contains("name=\"Overview\""));
    assert!(workbook_xml.contains("name=\"Benchmark\""));
    let mut overview_xml = String::new();
    workbook
        .by_name("xl/worksheets/sheet1.xml")
        .unwrap()
        .read_to_string(&mut overview_xml)
        .unwrap();
    assert!(overview_xml.contains("Metrics definition version"));
    assert!(overview_xml.contains(&summary["return_bps"].to_string()));
    let mut benchmark_xml = String::new();
    workbook
        .by_name("xl/worksheets/sheet5.xml")
        .unwrap()
        .read_to_string(&mut benchmark_xml)
        .unwrap();
    assert!(benchmark_xml.contains("per_bar_no_annualization"));
    for period in series {
        for key in [
            "start_ts",
            "end_ts",
            "portfolio_equity_start_raw",
            "portfolio_equity_end_raw",
            "benchmark_close_start_raw",
            "benchmark_close_end_raw",
            "portfolio_return_bps",
            "benchmark_return_bps",
            "active_return_bps",
        ] {
            assert!(
                benchmark_xml.contains(&period[key].to_string()),
                "XLSX {key}"
            );
        }
    }
}
