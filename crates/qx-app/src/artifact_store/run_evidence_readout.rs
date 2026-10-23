//! Application-owned projections used to construct and describe run EvidenceBundles.

use qx_spec::{RunEvidenceAssumption, RunEvidenceFidelity};

pub fn run_evidence_summary_str(
    summary: &serde_json::Value,
    pointer: &[&str],
) -> Result<String, String> {
    let mut cursor = summary;
    for key in pointer {
        cursor = cursor
            .get(key)
            .ok_or_else(|| format!("回测摘要缺 {}（运行证据包无法聚合）", pointer.join(".")))?;
    }
    cursor
        .as_str()
        .map(|value| value.to_string())
        .ok_or_else(|| format!("回测摘要的 {} 不是字符串", pointer.join(".")))
}
pub fn run_evidence_summary_u64(
    summary: &serde_json::Value,
    pointer: &[&str],
) -> Result<u64, String> {
    let mut cursor = summary;
    for key in pointer {
        cursor = cursor
            .get(key)
            .ok_or_else(|| format!("回测摘要缺 {}（运行证据包无法聚合）", pointer.join(".")))?;
    }
    cursor
        .as_u64()
        .ok_or_else(|| format!("回测摘要的 {} 不是无符号整数", pointer.join(".")))
}
pub fn run_evidence_assumption_rows(
    summary: &serde_json::Value,
) -> Result<Vec<RunEvidenceAssumption>, String> {
    let mut rows = vec![RunEvidenceAssumption {
        name: "matching_kernel".to_string(),
        value: summary
            .get("matching_kernel")
            .or_else(|| {
                summary
                    .get("risk_rules")
                    .and_then(|rules| rules.get("matching_kernel"))
            })
            .and_then(serde_json::Value::as_str)
            .ok_or("回测摘要缺 matching_kernel")?
            .to_string(),
        source: "kernel-self-described".to_string(),
    }];
    // 深度链没有 `FillModel`（内核不经过它），所以这一格缺席是正常的，不是漏填。
    if let Some(fill_model) = summary.get("fill_model") {
        let (model_name, version, source) = if let Some(legacy_name) = fill_model.as_str() {
            (legacy_name, None, "summary-declared")
        } else {
            let model_name = fill_model
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or("回测摘要的 fill_model.name 不是字符串")?;
            let source = fill_model
                .get("source")
                .and_then(serde_json::Value::as_str)
                .ok_or("回测摘要的 fill_model.source 不是字符串")?;
            (
                model_name,
                fill_model
                    .get("version")
                    .and_then(serde_json::Value::as_str),
                source,
            )
        };
        rows.push(RunEvidenceAssumption {
            name: "fill_model".to_string(),
            value: version.map_or_else(
                || model_name.to_string(),
                |version| format!("{model_name}@{version}"),
            ),
            source: source.to_string(),
        });
        for (field, name) in [
            ("data_tier", "fill_model_data_tier"),
            ("parameters", "fill_model_parameters"),
            ("assumption", "fill_model_assumption"),
        ] {
            // An empty parameter string is the model descriptor's representation of
            // "this model has no tunable parameters", not a usable assumption value.
            if let Some(value) = fill_model
                .get(field)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                rows.push(RunEvidenceAssumption {
                    name: name.to_string(),
                    value: value.to_string(),
                    source: "kernel-self-described".to_string(),
                });
            }
        }
    }
    if let Some(execution_model) = summary.get("execution_model") {
        rows.push(RunEvidenceAssumption {
            name: "orderbook_execution_model".to_string(),
            value: execution_model.to_string(),
            source: "kernel-self-described".to_string(),
        });
        if let Some(queue) = execution_model.get("queue_position") {
            rows.push(RunEvidenceAssumption {
                name: "queue_position_model".to_string(),
                value: queue.to_string(),
                source: "kernel-self-described".to_string(),
            });
        }
    }
    let (initial_cash, account_source) = if summary.get("accounts").is_some() {
        (
            format!(
                "primary={};reference={}",
                run_evidence_summary_str(summary, &["accounts", "primary_initial_cash"])?,
                run_evidence_summary_str(summary, &["accounts", "reference_initial_cash"])?
            ),
            run_evidence_summary_str(summary, &["accounts", "account_base_source"])?.to_string(),
        )
    } else {
        (
            run_evidence_summary_str(summary, &["account", "initial_cash_raw"])?.to_string(),
            run_evidence_summary_str(summary, &["account", "source"])?.to_string(),
        )
    };
    rows.push(RunEvidenceAssumption {
        name: "initial_cash_raw".to_string(),
        value: initial_cash,
        source: account_source,
    });
    rows.push(RunEvidenceAssumption {
        name: "risk_rule_set".to_string(),
        value: run_evidence_summary_str(summary, &["risk_rules", "rule_set_version"])?,
        source: run_evidence_summary_str(summary, &["risk_rules", "source"])?,
    });
    rows.push(RunEvidenceAssumption {
        name: "execution_costs".to_string(),
        value: run_evidence_summary_str(summary, &["execution_costs", "source"])?,
        source: "summary-declared".to_string(),
    });
    Ok(rows)
}
pub fn run_evidence_unverified_rows(summary: &serde_json::Value) -> Result<Vec<String>, String> {
    let kernel = summary
        .get("matching_kernel")
        .or_else(|| {
            summary
                .get("risk_rules")
                .and_then(|rules| rules.get("matching_kernel"))
        })
        .and_then(serde_json::Value::as_str)
        .ok_or("回测摘要缺 matching_kernel")?;
    let cost_source = run_evidence_summary_str(summary, &["execution_costs", "source"])?;
    Ok(vec![
        "本次运行未接触任何外部 venue：L3 沙盒档与 L4 生产档的证据不在本包内，本包只覆盖本地回测档"
            .to_string(),
        format!("撮合口径 matching_kernel={kernel} 是内核近似，未经真实成交回报与排队行为复核"),
        "数据集质量报告未聚合：本档输入的身份是 v1 数据集清单，不带质量报告（T1-6 才把它铺开）"
            .to_string(),
        format!("费用与滑点口径取自 {cost_source}，未经实盘成交复核"),
    ])
}
pub fn run_evidence_fidelity_classification(summary: &serde_json::Value) -> RunEvidenceFidelity {
    let groups = [
        (
            "observed",
            [
                "/input/kind",
                "/input/dataset_id",
                "/input/dataset_version",
                "/input/fingerprint",
                "/dataset_quality/manifest_fingerprint",
                "/dataset_quality/quality_report/row_count",
                "/bars",
                "/replay/events",
                "/replay/log_digest",
            ]
            .as_slice(),
        ),
        (
            "configured",
            [
                "/account/initial_cash_raw",
                "/account/source",
                "/risk_rules/rule_set_version",
                "/risk_rules/source",
                "/execution_costs/source",
            ]
            .as_slice(),
        ),
        (
            "estimated",
            [
                "/matching_kernel",
                "/fill_model",
                "/execution_model",
                "/model_descriptors",
                "/assumptions",
            ]
            .as_slice(),
        ),
        (
            "computed",
            [
                "/metrics/return_bps",
                "/metrics/max_drawdown_bps",
                "/metrics/fees_raw",
                "/metrics/turnover_raw",
                "/metrics/final_equity_raw",
                "/metrics/sharpe",
                "/metrics/sortino",
                "/metrics/calmar",
                "/metrics/win_rate",
                "/metrics/profit_factor",
                "/fills",
                "/rejected_orders",
            ]
            .as_slice(),
        ),
    ];
    let mut fidelity = RunEvidenceFidelity {
        source_artifact: "summary".into(),
        observed: Vec::new(),
        configured: Vec::new(),
        estimated: Vec::new(),
        computed: Vec::new(),
        absent: Vec::new(),
    };
    for (class, pointers) in groups {
        for pointer in pointers {
            if summary
                .pointer(pointer)
                .is_some_and(|value| !value.is_null())
            {
                match class {
                    "observed" => fidelity.observed.push((*pointer).into()),
                    "configured" => fidelity.configured.push((*pointer).into()),
                    "estimated" => fidelity.estimated.push((*pointer).into()),
                    "computed" => fidelity.computed.push((*pointer).into()),
                    _ => unreachable!("fidelity classes are a closed list"),
                }
            } else {
                fidelity.absent.push((*pointer).into());
            }
        }
    }
    fidelity
}

#[cfg(test)]
mod tests {
    use super::run_evidence_assumption_rows;
    use serde_json::json;

    fn base_summary(fill_model: serde_json::Value) -> serde_json::Value {
        json!({
            "matching_kernel": "bar-next-open-v1",
            "fill_model": fill_model,
            "account": {"initial_cash_raw": "100000", "source": "backtest-spec"},
            "risk_rules": {"rule_set_version": "risk-v1", "source": "runtime-config"},
            "execution_costs": {"source": "fee-schedule"},
        })
    }

    #[test]
    fn legacy_string_fill_model_keeps_its_declared_name() {
        let rows = run_evidence_assumption_rows(&base_summary(json!("NextBarOpen"))).unwrap();
        let fill = rows.iter().find(|row| row.name == "fill_model").unwrap();
        assert_eq!(fill.value, "NextBarOpen");
        assert_eq!(fill.source, "summary-declared");
    }

    #[test]
    fn structured_fill_model_still_requires_source() {
        let summary = base_summary(json!({"name": "NextBarOpen", "version": "v1"}));
        assert!(run_evidence_assumption_rows(&summary)
            .unwrap_err()
            .contains("fill_model.source"));
    }
}
