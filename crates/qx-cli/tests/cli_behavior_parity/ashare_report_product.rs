//! A-share report artifacts and same-input comparison must work through the real CLI.

use super::*;
use serde_json::{json, Value};
use std::io::Read;
use std::path::Path;

#[path = "benchmark.rs"]
mod benchmark;
#[path = "ashare_report_product/quality.rs"]
mod quality;

fn write_json(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap())
        .unwrap_or_else(|error| panic!("write fixture {}: {error}", path.display()));
}

fn run(cwd: &Path, args: &[String]) -> Output {
    Command::new(binary())
        .current_dir(cwd)
        .env("TEMP", cwd)
        .env("TMP", cwd)
        .env_remove("QX_DEPLOY_DIR")
        .env_remove("QX_PYTHON")
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run qx-cli {args:?}: {error}"))
}

fn output_text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn summary_files(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut summaries = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("read generated directory: {error}"))
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".summary.json"))
            {
                summaries.push(path);
            }
        }
    }
    summaries.sort();
    summaries
}

fn assert_evidence_recomputes_after_portable_export(
    cwd: &Path,
    report: &Value,
    runtime: &Path,
    run_index: usize,
) {
    let evidence_path = report["run_evidence"]["path"]
        .as_str()
        .expect("A-share report must expose its run EvidenceBundle path");
    let verified_output = run(
        cwd,
        &["evidence".into(), "verify-run".into(), evidence_path.into()],
    );
    assert_eq!(
        verified_output.status.code(),
        Some(0),
        "A-share EvidenceBundle must pass its integrity check: {}",
        String::from_utf8_lossy(&verified_output.stderr)
    );
    let verified: Value =
        serde_json::from_slice(&verified_output.stdout).expect("verify-run emits JSON");
    assert_eq!(verified["ok"], true);

    let portable_dir = cwd.join(format!("portable-ashare-evidence-{run_index}"));
    let exported_output = run(
        cwd,
        &[
            "evidence".into(),
            "export-run".into(),
            evidence_path.into(),
            "--output-dir".into(),
            portable_dir.to_string_lossy().into_owned(),
            "--runtime-config".into(),
            runtime.to_string_lossy().into_owned(),
        ],
    );
    assert_eq!(
        exported_output.status.code(),
        Some(0),
        "A-share EvidenceBundle must export as a portable package: {}",
        String::from_utf8_lossy(&exported_output.stderr)
    );
    let exported: Value =
        serde_json::from_slice(&exported_output.stdout).expect("export-run emits JSON");
    assert_eq!(exported["self_contained_files"], true);

    let portable_bundle = portable_dir.join("evidence.json");
    assert!(
        portable_bundle.is_file(),
        "portable EvidenceBundle is missing"
    );
    let portable_output = run(
        cwd,
        &[
            "evidence".into(),
            "verify-run".into(),
            portable_bundle.to_string_lossy().into_owned(),
            "--recompute".into(),
        ],
    );
    assert_eq!(
        portable_output.status.code(),
        Some(0),
        "portable A-share evidence must recompute after export: {}",
        String::from_utf8_lossy(&portable_output.stderr)
    );
    let portable: Value =
        serde_json::from_slice(&portable_output.stdout).expect("verify-run emits JSON");
    assert_eq!(portable["ok"], true);
    assert_eq!(portable["result_recomputed"], true);
    assert_eq!(
        portable["result_hash"], verified["result_hash"],
        "portable A-share recompute must preserve the original result hash"
    );
}

#[test]
fn ashare_runs_export_documents_and_compare_on_the_same_input() {
    let cwd = temp_dir("ashare-report-product", 1);
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli tests are two levels below the workspace root");
    let frame_path = cwd.join("bars.frame.json");
    let closes = (0..24)
        .map(|index| 10_000_000_000_i64 + ((index % 7) - 3) * 100_000_000)
        .collect::<Vec<_>>();
    let opens = (0..24)
        .map(|index| {
            if index == 0 {
                closes[0]
            } else {
                closes[index - 1]
            }
        })
        .collect::<Vec<_>>();
    let highs = (0..24)
        .map(|index| opens[index].max(closes[index]) + 100_000_000)
        .collect::<Vec<_>>();
    let lows = (0..24)
        .map(|index| opens[index].min(closes[index]) - 100_000_000)
        .collect::<Vec<_>>();
    let frame = json!({
        "instrument": "000001.SZSE",
        "source": "ashare-report-process-fixture-v1",
        "ts": (0..24).map(|index| 1_704_159_000_000_i64 + index * 60_000).collect::<Vec<_>>(),
        "open_raw": opens,
        "high_raw": highs,
        "low_raw": lows,
        "close_raw": closes,
        "volume_raw": vec![1_000_000_000_000_i64; 24]
    });
    write_json(&frame_path, &frame);

    let rules = cwd.join("ashare.rules.json");
    std::fs::copy(repo.join("deploy/qianxing.ashare.rules.json"), &rules)
        .expect("copy A-share rules into the portable fixture root");
    let spec = repo.join("deploy/qianxing.ashare.spot.spec.json");
    let calendar_path = cwd.join("calendar.json");
    write_json(
        &calendar_path,
        &json!({
            "calendar_id": "ashare-report-process-calendar-v1",
            "trading_days": ["2024-01-02"],
            "sessions": [["09:30", "11:30"], ["13:00", "15:00"]]
        }),
    );
    let actions_path = cwd.join("actions.json");
    write_json(&actions_path, &json!([]));

    let mut config: Value = serde_json::from_slice(
        &std::fs::read(repo.join("deploy/qianxing.runtime.ashare.example.json"))
            .expect("read A-share runtime template"),
    )
    .expect("A-share runtime template is JSON");
    config["strategy"]["ashare_rules_path"] = json!("ashare.rules.json");
    config["strategy"]["ashare_calendar_path"] = json!("calendar.json");
    config["strategy"]["ashare_actions_path"] = json!("actions.json");
    config["strategy"]["bars_snapshot_path"] = json!("bars.frame.json");
    config["strategy"]["dataset_bundle_path"] = Value::Null;
    config["strategy"]["builtin_fast_window"] = json!(2);
    config["strategy"]["builtin_slow_window"] = json!(4);
    let baseline_config = cwd.join("baseline.runtime.json");
    config["storage"]["data_dir"] = json!("data/baseline");
    write_json(&baseline_config, &config);
    config["strategy"]["builtin_fast_window"] = json!(3);
    config["strategy"]["builtin_slow_window"] = json!(6);
    config["storage"]["data_dir"] = json!("data/candidate");
    let candidate_config = cwd.join("candidate.runtime.json");
    write_json(&candidate_config, &config);

    let quality_manifest = quality::create_manifest(&cwd, &frame_path);
    quality::assert_builtin_manifest_binding(
        &cwd,
        &quality_manifest,
        &baseline_config,
        &frame_path,
        &spec,
    );

    let mut summaries = Vec::new();
    let mut observed_summaries = summary_files(&cwd);
    for runtime in [&baseline_config, &candidate_config] {
        let args = vec![
            "strategy".into(),
            "backtest".into(),
            runtime.to_string_lossy().into_owned(),
            frame_path.to_string_lossy().into_owned(),
            spec.to_string_lossy().into_owned(),
            "--dataset-manifest".into(),
            quality_manifest.to_string_lossy().into_owned(),
        ];
        let output = run(&cwd, &args);
        let (stdout, stderr) = output_text(&output);
        assert_eq!(
            output.status.code(),
            Some(0),
            "A-share backtest failed: {stderr}\n{stdout}"
        );
        assert!(
            stdout.contains("result_hash="),
            "backtest must report result hash: {stdout}"
        );
        let created = summary_files(&cwd);
        let summary = created
            .into_iter()
            .find(|path| !observed_summaries.contains(path))
            .expect("find newly written summary");
        observed_summaries.push(summary.clone());
        let payload: Value = serde_json::from_slice(&std::fs::read(&summary).unwrap())
            .expect("summary artifact is JSON");
        assert!(
            payload["model_descriptors"]
                .as_array()
                .is_some_and(|models| models.iter().any(|model| model
                    .as_str()
                    .is_some_and(|value| value.starts_with("AshareRules@")))),
            "the persisted run must disclose its A-share rule model: {payload}"
        );
        assert!(
            payload["rejection_reasons"]
                .as_array()
                .is_some_and(|reasons| {
                    reasons.iter().any(|reason| {
                        reason["reason"]
                            .as_str()
                            .is_some_and(|value| value.contains("T+1"))
                    })
                }),
            "the A-share fixture must exercise its T+1 rule: {payload}"
        );
        quality::assert_summary(&payload, 60_000, 24);
        summaries.push(summary);
    }
    assert_ne!(summaries[0], summaries[1]);
    quality::assert_drift_rejected(
        &cwd,
        &quality_manifest,
        &candidate_config,
        &frame_path,
        &spec,
    );

    for (run_index, (summary, runtime)) in summaries
        .iter()
        .zip([&baseline_config, &candidate_config])
        .enumerate()
    {
        let args = vec![
            "report".into(),
            summary.to_string_lossy().into_owned(),
            "--json".into(),
            "--html".into(),
            "--evidence".into(),
        ];
        let output = run(&cwd, &args);
        let (stdout, stderr) = output_text(&output);
        assert_eq!(
            output.status.code(),
            Some(0),
            "A-share report failed: {stderr}\n{stdout}"
        );
        let report: Value =
            serde_json::from_str(&stdout).expect("report --json emits one JSON value");
        assert_eq!(report["input_check"]["verdict"], "verified");
        assert_eq!(report["summary"]["schema_version"], 7);
        assert!(report["generated_artifacts"]["html"].as_str().is_some());
        assert!(report["run_evidence"]["path"].as_str().is_some());
        quality::assert_html_and_evidence_quality(&report);
        assert_evidence_recomputes_after_portable_export(&cwd, &report, runtime, run_index);
        for artifact in ["html", "equity_svg", "fills_svg", "monthly_svg"] {
            let path = Path::new(report["generated_artifacts"][artifact].as_str().unwrap());
            assert!(
                path.is_file(),
                "report artifact missing: {}",
                path.display()
            );
        }

        benchmark::assert_benchmark_report_product(&cwd, summary, &frame);

        let direct_xlsx = cwd.join(format!(
            "{}.report.xlsx",
            summary
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("run")
        ));
        let direct_args = vec![
            "report".into(),
            summary.to_string_lossy().into_owned(),
            "--json".into(),
            "--html".into(),
            "--xlsx".into(),
            direct_xlsx.to_string_lossy().into_owned(),
        ];
        let direct = run(&cwd, &direct_args);
        assert_eq!(direct.status.code(), Some(0), "A-share report rerun failed");
        let direct_json: Value = serde_json::from_slice(&direct.stdout)
            .expect("top-level A-share report rerun emits JSON");
        let saved_artifacts =
            ["html", "equity_svg", "fills_svg", "monthly_svg", "xlsx"].map(|name| {
                let path = Path::new(direct_json["generated_artifacts"][name].as_str().unwrap());
                (
                    name,
                    std::fs::read(path).expect("read top-level report artifact"),
                )
            });
        assert_eq!(
            direct_json["generated_artifacts"]["xlsx"],
            direct_xlsx.display().to_string()
        );
        quality::assert_xlsx_quality(&direct_xlsx);

        let alias_args = std::iter::once("run".to_owned())
            .chain(direct_args)
            .collect::<Vec<_>>();
        let alias = run(&cwd, &alias_args);
        let (alias_stdout, alias_stderr) = output_text(&alias);
        assert_eq!(
            alias.status.code(),
            Some(0),
            "run report A-share alias failed: {alias_stderr}"
        );
        let alias_json: Value =
            serde_json::from_str(&alias_stdout).expect("run report A-share alias emits JSON");
        assert_eq!(
            direct_json, alias_json,
            "run report and report A-share product fields diverged"
        );
        for (name, expected) in saved_artifacts {
            let path = Path::new(alias_json["generated_artifacts"][name].as_str().unwrap());
            assert_eq!(
                std::fs::read(path).expect("read run report artifact"),
                expected,
                "run report changed the A-share {name} bytes"
            );
        }
    }

    let comparison_html = cwd.join("comparison.html");
    let comparison_xlsx = cwd.join("comparison.xlsx");
    let compare_args = vec![
        "report".into(),
        summaries[0].to_string_lossy().into_owned(),
        "--against".into(),
        summaries[1].to_string_lossy().into_owned(),
        "--json".into(),
        "--html".into(),
        "--xlsx".into(),
        comparison_xlsx.to_string_lossy().into_owned(),
        "--output".into(),
        comparison_html.to_string_lossy().into_owned(),
    ];
    let comparison = run(&cwd, &compare_args);
    let (stdout, stderr) = output_text(&comparison);
    assert_eq!(
        comparison.status.code(),
        Some(0),
        "A-share comparison failed: {stderr}\n{stdout}"
    );
    let report: Value =
        serde_json::from_str(&stdout).expect("comparison --json emits one JSON value");
    assert_eq!(report["comparison_kind"], "same_input_backtest_runs");
    assert_eq!(
        report["generated_artifacts"]["xlsx"],
        comparison_xlsx.display().to_string()
    );
    let mut comparison_book = zip::ZipArchive::new(
        std::fs::File::open(&comparison_xlsx).expect("comparison XLSX exists"),
    )
    .expect("comparison XLSX is a ZIP package");
    let mut ranking_sheet = String::new();
    comparison_book
        .by_name("xl/worksheets/sheet2.xml")
        .unwrap()
        .read_to_string(&mut ranking_sheet)
        .unwrap();
    assert!(ranking_sheet.contains("return_bps"));
    assert!(ranking_sheet.contains("strategy-backtest"));
    let ranked = report["runs"]
        .as_array()
        .expect("comparison has runs array");
    assert_eq!(ranked.len(), 2);
    assert_eq!(ranked[0]["rank"], 1);
    assert_eq!(ranked[1]["rank"], 2);
    assert_eq!(ranked[0]["result_hash"].as_str().is_some(), true);
    assert_eq!(ranked[1]["result_hash"].as_str().is_some(), true);
    assert_eq!(ranked[0]["run_id"] == ranked[1]["run_id"], false);
    assert!(
        comparison_html.is_file(),
        "offline comparison HTML must be written"
    );
    let html = std::fs::read_to_string(&comparison_html).expect("read comparison HTML");
    assert!(html.contains("牵星回测横向对照"));
    assert!(html.contains("000001.SZSE"));
    assert!(
        html.contains("lang=\"zh-CN\"")
            && html.contains("name=\"viewport\"")
            && html.contains(".scroll{overflow-x:auto}")
            && html.contains("<div class=\"scroll\"><table>")
            && html.contains("原始收益排名")
            && html.contains("主动收益排名")
            && html.contains("模型假设")
            && html.contains("结果哈希")
            && html.contains("</table>")
            && !html.contains("<script")
            && !html.contains("src=\"http"),
        "comparison HTML must be a self-contained, mobile-aware analysis table"
    );
    benchmark::assert_comparison_benchmark_product(&cwd, &summaries);

    let artifacts = artifact_files(&cwd);
    assert!(artifacts.iter().any(|path| path.ends_with(".equity.csv")));
    assert!(artifacts.iter().any(|path| path.ends_with(".fills.csv")));

    // The historical top-level route and its `run` alias must preserve the same A-share product
    // contract, including result identity and persisted artifact bytes. Reuse one working directory
    // so any absolute paths in the runtime config are identical across the two invocations.
    let mut alias_config: Value = serde_json::from_slice(
        &std::fs::read(&baseline_config).expect("read baseline runtime config"),
    )
    .expect("baseline runtime config is JSON");
    let alias_data = cwd.join("data/entry-alias");
    alias_config["storage"]["data_dir"] = json!(alias_data.to_string_lossy());
    let alias_runtime = cwd.join("entry-alias.runtime.json");
    write_json(&alias_runtime, &alias_config);
    let input_args = [
        alias_runtime.to_string_lossy().into_owned(),
        frame_path.to_string_lossy().into_owned(),
        spec.to_string_lossy().into_owned(),
    ];
    let direct_args = std::iter::once("backtest".to_owned())
        .chain(input_args.iter().cloned())
        .collect::<Vec<_>>();
    let direct = run(&cwd, &direct_args);
    let (direct_stdout, direct_stderr) = output_text(&direct);
    assert_eq!(
        direct.status.code(),
        Some(0),
        "top-level A-share backtest failed: {direct_stderr}\n{direct_stdout}"
    );
    let direct_stdout = normalize_stdout(
        &direct_args.iter().map(String::as_str).collect::<Vec<_>>(),
        &cwd,
        &direct_stdout,
    );
    let direct_files = artifact_files(&alias_data);
    let direct_digests = artifact_digests(&alias_data, &direct_files);
    let read_json_artifact = |root: &Path, files: &[String], suffix: &str| {
        let path = files
            .iter()
            .find(|path| path.ends_with(suffix))
            .unwrap_or_else(|| panic!("backtest omitted {suffix}"));
        serde_json::from_slice::<Value>(
            &std::fs::read(root.join(path)).expect("read backtest product JSON"),
        )
        .expect("backtest product is JSON")
    };
    let direct_run_manifest = read_json_artifact(&alias_data, &direct_files, ".run.json");
    let direct_summary = read_json_artifact(&alias_data, &direct_files, ".summary.json");
    assert!(direct_stdout.contains("result_hash="));
    assert!(
        !direct_files.is_empty(),
        "backtest must persist run artifacts"
    );

    std::fs::remove_dir_all(&alias_data).expect("reset alias output directory");
    let run_args = std::iter::once("run".to_owned())
        .chain(direct_args.iter().cloned())
        .collect::<Vec<_>>();
    let aliased = run(&cwd, &run_args);
    let (aliased_stdout, aliased_stderr) = output_text(&aliased);
    assert_eq!(
        aliased.status.code(),
        Some(0),
        "run backtest A-share alias failed: {aliased_stderr}\n{aliased_stdout}"
    );
    let aliased_stdout = normalize_stdout(
        &run_args.iter().map(String::as_str).collect::<Vec<_>>(),
        &cwd,
        &aliased_stdout,
    );
    let aliased_files = artifact_files(&alias_data);
    let aliased_digests = artifact_digests(&alias_data, &aliased_files);
    assert_eq!(
        direct_stdout, aliased_stdout,
        "run backtest stdout diverged"
    );
    assert_eq!(
        direct_files, aliased_files,
        "run backtest artifact set diverged"
    );
    let result_hashes = |stdout: &str| {
        stdout
            .split_whitespace()
            .filter_map(|token| token.strip_prefix("result_hash="))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        result_hashes(&direct_stdout),
        result_hashes(&aliased_stdout),
        "run backtest result identity diverged"
    );
    for suffix in [".equity.csv", ".fills.csv", ".run.json"] {
        let direct = direct_digests
            .iter()
            .find(|(path, _)| path.ends_with(suffix))
            .unwrap_or_else(|| panic!("top-level backtest omitted {suffix}"));
        let aliased = aliased_digests
            .iter()
            .find(|(path, _)| path.ends_with(suffix))
            .unwrap_or_else(|| panic!("run backtest omitted {suffix}"));
        assert_eq!(direct, aliased, "run backtest {suffix} content diverged");
    }
    let aliased_run_manifest = read_json_artifact(&alias_data, &aliased_files, ".run.json");
    let aliased_summary = read_json_artifact(&alias_data, &aliased_files, ".summary.json");
    assert_eq!(direct_run_manifest, aliased_run_manifest);
    for field in [
        "result_hash",
        "metrics",
        "model_descriptors",
        "rejection_reasons",
    ] {
        assert_eq!(
            direct_summary[field], aliased_summary[field],
            "run backtest summary field {field} diverged"
        );
    }

    let _ = std::fs::remove_dir_all(cwd);
}
