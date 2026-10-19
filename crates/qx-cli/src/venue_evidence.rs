//! Venue EvidenceBundle producer backed by the durable external acceptance output.
//!
//! This intentionally accepts only a passed, credentialed, non-dry-run testnet/sandbox
//! acceptance window with the full submit/retry/recovery/reconcile chain. It does not
//! turn Paper or a local run into venue evidence, and it never embeds raw stage output.

use qx_spec::{
    CapabilityLevel, EvidenceBundle, EvidenceClaim, EvidenceEnvironment, EvidenceWindow,
    FoundationDocument, VenueEvidencePayload,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn read_json(path: &Path, label: &str) -> Result<Value, String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取 {label} 失败 {}: {error}", path.display()))?;
    serde_json::from_str(&payload).map_err(|error| format!("解析 {label} 失败: {error}"))
}

fn required_str<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("venue 验收结果缺少非空 {field}"))
}

fn redact_config(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                if matches!(
                    key.to_ascii_lowercase().as_str(),
                    "api_key" | "secret" | "password"
                ) {
                    *child = Value::String("[REDACTED]".into());
                } else {
                    redact_config(child);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_config),
        _ => {}
    }
}

fn stage_name<'a>(stage: &'a Value) -> Result<&'a str, String> {
    stage
        .get("stage")
        .and_then(Value::as_str)
        .ok_or_else(|| "venue 验收阶段缺少 stage 名称".into())
}

fn validate_live_check(stage: &Value) -> Result<(), String> {
    let output = stage
        .get("output_tail")
        .and_then(Value::as_array)
        .ok_or("venue live-check 阶段缺少只读结果")?;
    let safety_report = output.iter().find_map(|line| {
        let line = line.as_str()?;
        let report: Value = serde_json::from_str(line).ok()?;
        (report.get("network_accessed").and_then(Value::as_bool) == Some(false)
            && report.get("orders_sent").and_then(Value::as_bool) == Some(false))
        .then_some(report)
    });
    safety_report.map(|_| ()).ok_or_else(|| {
        "venue live-check 没有证明 network_accessed=false 且 orders_sent=false".into()
    })
}

fn validate_passed_window(result: &Value) -> Result<(&'static str, &'static str), String> {
    if required_str(result, "outcome")? != "pass" {
        return Err("只有 outcome=pass 的 venue 验收窗口可生成 L3 EvidenceBundle".into());
    }
    if result.get("credentials_present").and_then(Value::as_bool) != Some(true) {
        return Err("venue 验收结果未证明凭据已提供，拒绝生成 L3 证据".into());
    }
    if result.get("send_orders").and_then(Value::as_bool) != Some(true) {
        return Err(
            "venue 验收必须以 --send-orders 完成真实沙盒订单，dry-run 不能生成 L3 证据".into(),
        );
    }

    let venue = required_str(result, "venue")?;
    let (venue_id, network, submit_stage, recovery_stage) = match venue {
        "binance" => (
            "binance-testnet",
            "testnet",
            "binance-submit-order",
            "binance-worker",
        ),
        "okx" => ("okx-sandbox", "sandbox", "ccxt-submit-order", "ccxt-worker"),
        other => return Err(format!("venue={other} 没有登记的 L3 证据生产口径")),
    };

    let stages = result
        .get("stages")
        .and_then(Value::as_array)
        .ok_or("venue 验收结果缺少 stages 数组")?;
    let mut seen = std::collections::BTreeSet::new();
    let mut seen_any = std::collections::BTreeSet::new();
    let mut submit_count = 0usize;
    let mut expected_rejection_count = 0usize;
    for stage in stages {
        let name = stage_name(stage)?;
        let code = stage
            .get("exit_code")
            .and_then(Value::as_i64)
            .ok_or_else(|| format!("venue 验收阶段 {name} 没有整数退出码"))?;
        seen_any.insert(name.to_string());
        if name == "live-check" {
            validate_live_check(stage)?;
        }
        if name == submit_stage {
            submit_count += 1;
            if stage
                .get("expected_idempotent_rejection")
                .and_then(Value::as_bool)
                == Some(true)
            {
                expected_rejection_count += 1;
                if code == 0
                    || stage
                        .get("idempotency_rejection_confirmed")
                        .and_then(Value::as_bool)
                        != Some(true)
                {
                    return Err("重复提交阶段没有可核对的幂等拒绝事实".into());
                }
                continue;
            }
        }
        if name != "live-check" && code != 0 {
            return Err(format!(
                "venue 验收阶段 {name} 退出码为 {code}，不能生成 L3 证据"
            ));
        }
        if name == submit_stage && code != 0 {
            return Err("真实 venue 首次提交必须成功".into());
        }
        if code == 0 {
            seen.insert(name.to_string());
        }
    }
    let mut required = vec![
        "runtime-check",
        submit_stage,
        recovery_stage,
        "reconcile",
        "status",
    ];
    let venue_specific = match venue {
        "binance" => vec!["binance-public-probe", "binance-private-probe"],
        "okx" => vec!["ccxt-market-spec", "ccxt-fetch-ohlcv"],
        _ => return Err(format!("venue={venue} 没有登记的验收阶段")),
    };
    required.extend(venue_specific);
    let missing = required
        .iter()
        .filter(|name| !seen.contains(**name))
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "venue 验收窗口缺少成功阶段：{}",
            missing.join(", ")
        ));
    }
    if !seen_any.contains("live-check") {
        return Err("venue 验收窗口缺少 live-check 安全断言".into());
    }
    if submit_count != 2 {
        return Err("venue 验收窗口必须包含原始提交与幂等重复提交两次记录".into());
    }
    if expected_rejection_count != 1 {
        return Err("venue 验收窗口必须恰有一次经确认的幂等重复提交拒绝".into());
    }
    Ok((venue_id, network))
}

fn epoch_ms(result: &Value, field: &str) -> Result<u64, String> {
    result
        .get(field)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("venue 验收结果缺少有效 {field}"))
}

fn hash_sources(paths: &[PathBuf]) -> Result<String, String> {
    let mut sources = Vec::with_capacity(paths.len());
    for path in paths {
        sources.push((
            path.file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| format!("证据源文件名不是 UTF-8: {}", path.display()))?
                .to_string(),
            qx_strategy::file_digest::sha256_file_hex(path)?,
        ));
    }
    sources.sort();
    let canonical =
        serde_json::to_vec(&sources).map_err(|error| format!("摘要序列化失败: {error}"))?;
    Ok(qx_strategy::c_api::sha256_hex(&canonical))
}

pub(crate) fn pack_acceptance(acceptance_dir: &Path, operator: &str) -> Result<(), String> {
    if operator.trim().is_empty() {
        return Err("--operator 必须填写可追责的操作人标识".into());
    }
    let acceptance_dir = acceptance_dir
        .canonicalize()
        .map_err(|error| format!("验收目录不可读 {}: {error}", acceptance_dir.display()))?;
    let result_path = acceptance_dir.join("result.json");
    let status_path = acceptance_dir.join("status.json");
    let config_path = acceptance_dir.join("qianxing.runtime.acceptance.json");
    for (path, label) in [
        (&result_path, "result.json"),
        (&status_path, "status.json"),
        (&config_path, "验收运行配置"),
    ] {
        if !path.is_file() {
            return Err(format!("venue 证据缺少 {label}: {}", path.display()));
        }
    }

    let result = read_json(&result_path, "result.json")?;
    let (venue_id, network) = validate_passed_window(&result)?;
    let window = EvidenceWindow {
        start_ms: epoch_ms(&result, "started_at_unix_ms")?,
        end_ms: epoch_ms(&result, "finished_at_unix_ms")?,
    };
    if window.start_ms >= window.end_ms {
        return Err("venue 验收窗口时间顺序非法".into());
    }

    let mut config = read_json(&config_path, "验收运行配置")?;
    redact_config(&mut config);
    let redacted_config =
        serde_json::to_vec(&config).map_err(|error| format!("脱敏配置序列化失败: {error}"))?;
    let redacted_config_digest = qx_strategy::c_api::sha256_hex(&redacted_config);
    let result_digest = hash_sources(&[result_path.clone(), status_path.clone(), config_path])?;
    let stages = result
        .get("stages")
        .and_then(Value::as_array)
        .ok_or("venue 验收结果缺少 stages 数组")?;

    let payload = VenueEvidencePayload {
        claim: EvidenceClaim {
            summary: format!("{venue_id} 真实订单、恢复与对账验收窗口"),
            capability_level: CapabilityLevel::L3,
        },
        venue_id: venue_id.into(),
        window,
        environment: EvidenceEnvironment {
            host: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            runtime_version: env!("CARGO_PKG_VERSION").into(),
            network: network.into(),
        },
        redacted_config_digest,
        logs: vec!["result.json".into(), "status.json".into()],
        fact_count: stages.len() as u64 + 1,
        result_digest,
        operator: operator.trim().to_string(),
    };
    let bundle = EvidenceBundle::venue(payload);
    let encoded = bundle.to_json()?;
    // Re-enter the canonical read funnel so the producer cannot write a bundle that the CLI
    // itself would reject when signing or verifying it later.
    EvidenceBundle::from_json(&encoded)?;
    let output_path = acceptance_dir.join("venue.evidence.json");
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)
        .map_err(|error| {
            format!(
                "创建 venue EvidenceBundle 失败 {}: {error}",
                output_path.display()
            )
        })?;
    use std::io::Write;
    if let Err(error) = output.write_all(format!("{encoded}\n").as_bytes()) {
        drop(output);
        let _ = std::fs::remove_file(&output_path);
        return Err(format!(
            "写入 venue EvidenceBundle 失败 {}: {error}",
            output_path.display()
        ));
    }
    println!(
        "[qx-cli · CLI] venue EvidenceBundle: {}",
        output_path.display()
    );
    Ok(())
}
