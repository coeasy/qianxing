//! Grafana 概览面板（易用性 P3 / 工业级 §17 可观测）只引用**真实存在**的 Prometheus 指标。
//!
//! `qx-api` 的 `/metrics` 与 `worker-metrics/*.prom` 实际暴露的指标名以 `qx_api_` /
//! `qx_control_retired_` / `qx_worker_` / `qx_pipeline_` 四族前缀存在（见
//! `crates/qx-api/tests/prometheus_exposition.rs` 与 `crates/qx-cli/src/pipeline_metrics_report.rs`）。
//! 面板里一旦出现拼写错或引用尚未接线的指标，监控就会「看着在跑实际是瞎的」——这条用例把
//! 所有 `expr` 抽出来，逐个要求指标名落在已知前缀内，避免 dashboard 与真实出口悄悄分叉。

use std::path::{Path, PathBuf};

/// 仓库真实存在的指标前缀；与 Prometheus 出口源码保持一致。
const KNOWN_PREFIXES: &[&str] = &[
    "qx_api_",
    "qx_control_retired_",
    "qx_worker_",
    "qx_pipeline_",
];

fn dashboard_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("grafana")
        .join("qianxing-overview.json")
}

/// 递归收集所有面板 target 的 `expr` 字符串。
fn collect_exprs(node: &serde_json::Value, out: &mut Vec<String>) {
    if let Some(expr) = node.get("expr").and_then(serde_json::Value::as_str) {
        out.push(expr.to_string());
    }
    for child in node.as_array().into_iter().flatten().chain(
        node.as_object()
            .into_iter()
            .flat_map(|object| object.values()),
    ) {
        collect_exprs(child, out);
    }
}

#[test]
fn grafana_dashboard_is_valid_json_and_references_real_metrics_only() {
    let path = dashboard_path();
    assert!(
        path.is_file(),
        "Grafana 概览面板必须存在: {}",
        path.display()
    );
    let payload = std::fs::read_to_string(&path).expect("读取 Grafana 面板失败");
    let dashboard: serde_json::Value =
        serde_json::from_str(&payload).expect("Grafana 面板必须是合法 JSON");
    assert_eq!(
        dashboard.get("uid").and_then(serde_json::Value::as_str),
        Some("qianxing-overview"),
        "面板 uid 必须稳定，便于 Grafana 配置供应"
    );

    let mut exprs = Vec::new();
    collect_exprs(&dashboard, &mut exprs);
    assert!(!exprs.is_empty(), "面板至少要有一个查询");

    for expr in &exprs {
        // 抽 expr 里所有「像指标名」的 token（以 `qx_` 开头），逐个要求落在已知前缀内；
        // `sum` / `rate` / `time` 这类 PromQL 函数与 `5m` 这样的区间不应被当指标校验。
        for token in expr.split(|c: char| !c.is_alphanumeric() && c != '_') {
            if token.starts_with("qx_") {
                assert!(
                    KNOWN_PREFIXES
                        .iter()
                        .any(|prefix| token.starts_with(prefix)),
                    "面板 expr 引用了未知指标 {token}（expr={expr}）；只许用真实存在的 qx_* 指标"
                );
            }
        }
    }
}
