use super::*;
use serde_json::{json, Value};
use std::io::Read;

pub(super) fn assert_benchmark_report_product(cwd: &Path, summary: &Path, frame: &Value) {
    let benchmark_path = cwd.join("benchmark-bars.json");
    let benchmark_closes = (0..24)
        .map(|index| 4_000_000_000_i64 + index * 5_000_000)
        .collect::<Vec<_>>();
    let benchmark = json!({
        "instrument": "000300.SSE",
        "source": "benchmark-fixture-raw-close-v1",
        "ts": frame["ts"],
        "open_raw": benchmark_closes,
        "high_raw": (0..24).map(|index| 4_010_000_000_i64 + index * 5_000_000).collect::<Vec<_>>(),
        "low_raw": (0..24).map(|index| 3_990_000_000_i64 + index * 5_000_000).collect::<Vec<_>>(),
        "close_raw": (0..24).map(|index| 4_000_000_000_i64 + index * 5_000_000).collect::<Vec<_>>(),
        "volume_raw": vec![1_000_000_000_000_i64; 24]
    });
    write_json(&benchmark_path, &benchmark);
    let benchmark_output = cwd.join("benchmark-report.html");
    let benchmark_csv = cwd.join(format!(
        "{}.benchmark.csv",
        summary
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("run")
    ));
    let benchmark_xlsx = cwd.join(format!(
        "{}.benchmark.xlsx",
        summary
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("run")
    ));
    let benchmark_args = vec![
        "report".into(),
        summary.to_string_lossy().into_owned(),
        "--benchmark-bars".into(),
        benchmark_path.to_string_lossy().into_owned(),
        "--benchmark-csv".into(),
        benchmark_csv.to_string_lossy().into_owned(),
        "--xlsx".into(),
        benchmark_xlsx.to_string_lossy().into_owned(),
        "--json".into(),
        "--html".into(),
        "--output".into(),
        benchmark_output.to_string_lossy().into_owned(),
        "--evidence".into(),
    ];
    let benchmark_report = run(&cwd, &benchmark_args);
    let (benchmark_stdout, benchmark_stderr) = output_text(&benchmark_report);
    assert_eq!(
        benchmark_report.status.code(),
        Some(0),
        "A-share benchmark report failed: {benchmark_stderr}\n{benchmark_stdout}"
    );
    let benchmark_json: Value =
        serde_json::from_slice(&benchmark_report.stdout).expect("benchmark comparison emits JSON");
    assert_eq!(
        benchmark_json["benchmark_analysis"]["alignment"],
        "exact_timestamp"
    );
    assert_eq!(
        benchmark_json["benchmark_analysis"]["period_basis"],
        "per_bar_no_annualization"
    );
    assert_eq!(
        benchmark_json["benchmark_analysis"]["benchmark"]["price_basis"],
        "raw_close_unadjusted"
    );
    assert_eq!(benchmark_json["benchmark_analysis"]["aligned_periods"], 23);
    let benchmark_csv_text = std::fs::read_to_string(&benchmark_csv).expect("benchmark CSV exists");
    assert_eq!(benchmark_csv_text.lines().count(), 24);
    assert!(benchmark_csv_text
        .lines()
        .next()
        .unwrap()
        .contains("active_return_bps"));
    assert!(benchmark_csv_text
        .lines()
        .next()
        .unwrap()
        .contains("price_basis"));
    assert_eq!(
        benchmark_json["generated_artifacts"]["benchmark_csv"],
        benchmark_csv.display().to_string()
    );
    assert_eq!(
        benchmark_json["generated_artifacts"]["xlsx"],
        benchmark_xlsx.display().to_string()
    );
    let workbook_file = std::fs::File::open(&benchmark_xlsx).expect("XLSX workbook exists");
    let mut workbook = zip::ZipArchive::new(workbook_file).expect("XLSX is a valid ZIP package");
    let mut workbook_xml = String::new();
    workbook
        .by_name("xl/workbook.xml")
        .unwrap()
        .read_to_string(&mut workbook_xml)
        .unwrap();
    assert!(workbook_xml.contains("name=\"Overview\""));
    assert!(workbook_xml.contains("name=\"Benchmark\""));
    let mut benchmark_sheet = String::new();
    workbook
        .by_name("xl/worksheets/sheet5.xml")
        .unwrap()
        .read_to_string(&mut benchmark_sheet)
        .unwrap();
    assert!(benchmark_sheet.contains("raw_close_unadjusted"));
    let benchmark_xlsx_bytes = std::fs::read(&benchmark_xlsx).unwrap();
    let evidence_path = benchmark_json["run_evidence"]["path"]
        .as_str()
        .expect("benchmark evidence path")
        .to_owned();
    let evidence_payload: Value = serde_json::from_slice(
        &std::fs::read(&evidence_path).expect("benchmark evidence bundle exists"),
    )
    .expect("benchmark evidence is JSON");
    assert_eq!(
        evidence_payload.pointer("/generation/run/report_analysis/kind"),
        Some(&json!("benchmark"))
    );
    assert_eq!(
        evidence_payload.pointer("/generation/run/report_analysis/input_fingerprint"),
        benchmark_json.pointer("/benchmark_analysis/benchmark/fingerprint")
    );
    let verify_args = vec!["evidence".into(), "verify-run".into(), evidence_path];
    let verified = run(&cwd, &verify_args);
    let (verified_stdout, verified_stderr) = output_text(&verified);
    assert_eq!(
        verified.status.code(),
        Some(0),
        "benchmark evidence verification failed: {verified_stderr}\n{verified_stdout}"
    );
    let run_evidence_path = benchmark_json["run_evidence"]["path"]
        .as_str()
        .expect("benchmark evidence path");
    let portable_dir = cwd.join(format!(
        "benchmark-portable-evidence-{}",
        summary
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("run")
    ));
    let export_args = vec![
        "evidence".into(),
        "export-run".into(),
        run_evidence_path.into(),
        "--output-dir".into(),
        portable_dir.to_string_lossy().into_owned(),
    ];
    let exported = run(&cwd, &export_args);
    let (export_stdout, export_stderr) = output_text(&exported);
    assert_eq!(
        exported.status.code(),
        Some(0),
        "benchmark evidence export failed: {export_stderr}\n{export_stdout}"
    );
    let portable_bundle = portable_dir.join("evidence.json");
    let portable_verify_args = vec![
        "evidence".into(),
        "verify-run".into(),
        portable_bundle.to_string_lossy().into_owned(),
    ];
    let portable_verified = run(&cwd, &portable_verify_args);
    let (portable_stdout, portable_stderr) = output_text(&portable_verified);
    assert_eq!(
        portable_verified.status.code(),
        Some(0),
        "portable benchmark evidence verification failed: {portable_stderr}\n{portable_stdout}"
    );
    assert!(benchmark_output.is_file());
    let benchmark_html = std::fs::read_to_string(&benchmark_output).unwrap();
    assert!(benchmark_html.contains("基准对照：000300.SSE"));
    let benchmark_bytes = std::fs::read(&benchmark_output).unwrap();
    let benchmark_alias_args = std::iter::once("run".to_owned())
        .chain(benchmark_args.clone())
        .collect::<Vec<_>>();
    let benchmark_alias = run(&cwd, &benchmark_alias_args);
    assert_eq!(
        benchmark_alias.status.code(),
        Some(0),
        "run report --benchmark-bars alias failed"
    );
    let benchmark_alias_json: Value = serde_json::from_slice(&benchmark_alias.stdout)
        .expect("run report benchmark alias emits JSON");
    assert_eq!(benchmark_alias_json, benchmark_json);
    assert_eq!(std::fs::read(&benchmark_output).unwrap(), benchmark_bytes);
    assert_eq!(
        std::fs::read(&benchmark_xlsx).unwrap(),
        benchmark_xlsx_bytes
    );
}

pub(super) fn assert_comparison_benchmark_product(cwd: &Path, summaries: &[PathBuf]) {
    let html = cwd.join("comparison-benchmark.html");
    let csv = cwd.join("comparison-benchmark.csv");
    let xlsx = cwd.join("comparison-benchmark.xlsx");
    let mut args = vec![
        "report".into(),
        summaries[0].to_string_lossy().into_owned(),
        "--against".into(),
        summaries[1].to_string_lossy().into_owned(),
        "--benchmark-bars".into(),
        cwd.join("benchmark-bars.json")
            .to_string_lossy()
            .into_owned(),
        "--benchmark-csv".into(),
        csv.to_string_lossy().into_owned(),
        "--xlsx".into(),
        xlsx.to_string_lossy().into_owned(),
        "--html".into(),
        "--output".into(),
        html.to_string_lossy().into_owned(),
        "--json".into(),
    ];
    let output = run(cwd, &args);
    let (stdout, stderr) = output_text(&output);
    assert_eq!(
        output.status.code(),
        Some(0),
        "comparison benchmark failed: {stderr}\n{stdout}"
    );
    let report: Value =
        serde_json::from_slice(&output.stdout).expect("comparison benchmark emits JSON");
    assert_eq!(
        report["comparison_kind"],
        "same_input_backtest_runs_vs_benchmark"
    );
    assert_eq!(
        report["benchmark_ranking"],
        "active_return_bps_desc,max_drawdown_bps_asc,run_id_asc"
    );
    let runs = report["runs"].as_array().expect("comparison run list");
    assert_eq!(runs.len(), 2);
    let mut benchmark_ranks = runs
        .iter()
        .map(|run| run["benchmark_rank"].as_u64().unwrap())
        .collect::<Vec<_>>();
    benchmark_ranks.sort_unstable();
    assert_eq!(benchmark_ranks, [1, 2]);
    for run in runs {
        assert_eq!(run["benchmark"]["instrument"], "000300.SSE");
        assert_eq!(run["benchmark"]["alignment"], "exact_timestamp");
        assert!(run["benchmark"]["active_return_bps"].is_number());
        assert!(run["benchmark"]["tracking_error_bps"].is_number());
        assert!(run["benchmark"]["information_ratio"].is_number());
        assert!(run["benchmark_rank"].is_number());
    }
    let csv_text = std::fs::read_to_string(csv).expect("combined benchmark CSV");
    assert_eq!(csv_text.lines().count(), 47);
    for run in runs {
        assert!(csv_text.contains(run["run_id"].as_str().unwrap()));
    }
    let html_text = std::fs::read_to_string(html).expect("comparison benchmark HTML");
    assert!(html_text.contains("主动收益"));
    assert!(html_text.contains("raw_close_unadjusted"));
    assert!(html_text.contains("基准指纹"));
    assert!(html_text.contains("主动收益排名"));
    let mut workbook = zip::ZipArchive::new(std::fs::File::open(xlsx).unwrap()).unwrap();
    let mut ranking = String::new();
    workbook
        .by_name("xl/worksheets/sheet2.xml")
        .unwrap()
        .read_to_string(&mut ranking)
        .unwrap();
    assert!(ranking.contains("active_return_bps"));
    assert!(ranking.contains("benchmark_fingerprint"));
    assert!(ranking.contains("benchmark_price_basis"));
    assert!(ranking.contains("benchmark_rank"));
    args.clear();
}
