//! WP-1.1: every checked-in blessed summary is either current or hits the named migration refusal.

use super::*;

#[test]
fn blessed_artifacts_are_current_or_explicitly_refused_with_migration_command() {
    fn visit(path: &Path, summaries: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(path).expect("读取 blessed 目录") {
            let entry = entry.expect("读取 blessed 目录项");
            let child = entry.path();
            if child.is_dir() {
                visit(&child, summaries);
            } else if child
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".summary.json"))
            {
                summaries.push(child);
            }
        }
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut summaries = Vec::new();
    visit(&root.join("deploy").join("data"), &mut summaries);
    summaries.sort();
    assert_eq!(
        summaries.len(),
        16,
        "blessed summary 集合发生变化，需复核治理范围"
    );

    for path in summaries {
        let payload = std::fs::read_to_string(&path).expect("读取 blessed summary");
        let summary: serde_json::Value = serde_json::from_str(&payload).expect("summary JSON 有效");
        let schema = summary["schema_version"].as_i64();
        if matches!(schema, Some(5 | 6)) {
            continue;
        }
        let refusal =
            require_current_summary_schema(&summary).expect_err("旧世代 summary 必须显式拒绝");
        assert!(refusal.contains("已拒绝"), "{}: {refusal}", path.display());
        assert!(
            refusal.contains("qx-cli backtest <runtime.json> [<bar-frame.json>]"),
            "{} 未给出可执行迁移命令: {refusal}",
            path.display()
        );
    }
}

#[test]
fn legacy_summary_refusal_precedes_html_and_evidence_side_effects() {
    let root = temp_cli_case_dir("legacy-summary-refusal");
    let summary_path = root.join("legacy.summary.json");
    std::fs::write(&summary_path, r#"{"schema_version":1}"#).unwrap();
    let html_path = root.join("reports").join("legacy.html");
    let error = run_report_with_output(&summary_path, true, true, true, Some(&html_path), &[])
        .expect_err("旧 schema 必须在报告产物写入前拒绝");
    assert!(error.contains("qx-cli backtest <runtime.json> [<bar-frame.json>]"));
    assert!(!html_path.exists(), "拒绝前不应创建 HTML");
    assert!(
        !root.join("legacy.run-evidence.json").exists(),
        "拒绝前不应创建 evidence"
    );
    let _ = std::fs::remove_dir_all(root);
}
