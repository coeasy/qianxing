//! 运行证据包构建器（QX-DEV-PLAN-2026-10-10 阶段 1 / T1-1）。
//!
//! 退出门 G1 第一条要求「同一运行可由 RunManifest **离线复算**」。复算的前提是**有人把复算需要
//! 的东西凑齐**：哪份 RunManifest、哪几份产物、哪份输入、这次跑的是哪套配置与哪个策略、撮合与
//! 费用口径是什么、哪些东西**没**被验证过、以及复算该按什么顺序做。这些事实此前散在摘要、清单、
//! 记录与 stdout 里，读者要自己拼——本模块把它们聚成一份 `RunEvidenceBundle`。
//!
//! 三条口径：
//! ① **只聚合，不重算**。这里的每一个数都从已经落盘的产物里搬过来（清单的 `result_hash`、
//!    摘要的 `bars`、记录的 `artifact_refs`……），本模块不重跑回测、不算指标、不重算哈希。
//!    产物摘要的可信度由 `report` 的复核链负责（`recompute_declared_backtest_input` 已经
//!    「摘要签发的摘要 + 逐个重算 SHA-256」），因此本模块只在**复核通过之后**才被调用。
//! ② **走统一规格漏斗写回**。写盘前用 `RunEvidenceBundle::from_json` 读回一遍，避免写侧绕过
//!    对象校验或 schema 版本漂移——与 `persist_verified_run_record` 同一条理由。
//! ③ **未验证清单是必填的**。这份包由本地回测产出，它**没有**任何外部 venue 证据；把这一格
//!    写空就是在盖一个单一 `verified` 标签，正是 T1-7 明令禁止的形态。这里的每一条都如实点名
//!    缺席的是哪一类证据，而不是写一句「仅供参考」。

use crate::*;
use qx_spec::{
    ArtifactRef, FoundationDocument, RunEvidenceAssumption, RunEvidenceBuild, RunEvidenceBundle,
    RunEvidenceDataset, RunEvidenceIdentity, RunEvidenceRecompute, RunEvidenceRun,
    RunEvidenceVerification, RunRecord, RUN_EVIDENCE_SCHEMA_VERSION,
};

/// 从摘要里取一个必填字符串格；缺失时点名是哪一格，而不是报一句泛泛的解析失败。
fn summary_str(summary: &serde_json::Value, pointer: &[&str]) -> Result<String, String> {
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

/// 从摘要里取一个必填无符号整数格。
fn summary_u64(summary: &serde_json::Value, pointer: &[&str]) -> Result<u64, String> {
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

/// 模型假设清单：每一条都点名「这个取值从哪来」。
///
/// 这里刻意**不**从 `model_descriptors` 里猜：那份是内核自述（带参数与假设），措辞随内核演进，
/// 拿它当 `value` 只会让假设清单跟着内核文案漂移。这里只搬摘要里已经结构化的四组口径。
fn assumption_rows(summary: &serde_json::Value) -> Result<Vec<RunEvidenceAssumption>, String> {
    let mut rows = vec![RunEvidenceAssumption {
        name: "matching_kernel".to_string(),
        value: summary_str(summary, &["matching_kernel"])?,
        source: "kernel-self-described".to_string(),
    }];
    // 深度链没有 `FillModel`（内核不经过它），所以这一格缺席是正常的，不是漏填。
    if let Some(fill_model) = summary.get("fill_model") {
        rows.push(RunEvidenceAssumption {
            name: "fill_model".to_string(),
            value: fill_model
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or("回测摘要的 fill_model.name 不是字符串")?
                .to_string(),
            source: fill_model
                .get("source")
                .and_then(serde_json::Value::as_str)
                .ok_or("回测摘要的 fill_model.source 不是字符串")?
                .to_string(),
        });
    }
    rows.push(RunEvidenceAssumption {
        name: "initial_cash_raw".to_string(),
        value: summary_str(summary, &["account", "initial_cash_raw"])?,
        source: summary_str(summary, &["account", "source"])?,
    });
    rows.push(RunEvidenceAssumption {
        name: "risk_rule_set".to_string(),
        value: summary_str(summary, &["risk_rules", "rule_set_version"])?,
        source: summary_str(summary, &["risk_rules", "source"])?,
    });
    rows.push(RunEvidenceAssumption {
        name: "execution_costs".to_string(),
        value: summary_str(summary, &["execution_costs", "source"])?,
        source: "summary-declared".to_string(),
    });
    Ok(rows)
}

/// 未验证清单：如实点名**缺席的是哪一类证据**。
///
/// 这不是免责声明，而是 T1-7 要的「四类可信度分离」里最容易被糊掉的一格：本地回测跑得再干净，
/// 它也没有外部 venue 的成交回报、没有真实滑点、没有实盘对手方行为。
fn unverified_rows(summary: &serde_json::Value) -> Result<Vec<String>, String> {
    let kernel = summary_str(summary, &["matching_kernel"])?;
    let cost_source = summary_str(summary, &["execution_costs", "source"])?;
    Ok(vec![
        "本次运行未接触任何外部 venue：L3 沙盒档与 L4 生产档的证据不在本包内，本包只覆盖本地回测档"
            .to_string(),
        format!("撮合口径 matching_kernel={kernel} 是内核近似，未经真实成交回报与排队行为复核"),
        "数据集质量报告未聚合：本档输入的身份是 v1 数据集清单，不带质量报告（T1-6 才把它铺开）"
            .to_string(),
        format!("费用与滑点口径取自 {cost_source}，未经实盘成交复核"),
    ])
}

/// 把一次已完成回测的全部可复核事实聚成一份 `RunEvidenceBundle` 并落盘到 `<stem>.evidence.json`。
///
/// 调用点必须在 `recompute_declared_backtest_input` 通过之后：那一步已经重算过每一份产物的
/// SHA-256，`artifact_digests_verified=true` 才是**真的**而不是一句声明。
pub(crate) fn write_run_evidence(
    summary_path: &Path,
    summary: &serde_json::Value,
) -> Result<PathBuf, String> {
    let manifest_path = summary_str(summary, &["run_manifest"])?;
    let manifest =
        RunManifest::from_json(&std::fs::read_to_string(&manifest_path).map_err(|error| {
            format!("读取运行证据包的 RunManifest 失败 {manifest_path}: {error}")
        })?)?;
    let record_path = summary_str(summary, &["run_record"])?;
    let record = RunRecord::from_json(
        &std::fs::read_to_string(&record_path)
            .map_err(|error| format!("读取运行证据包的 RunRecord 失败 {record_path}: {error}"))?,
    )?;
    let artifacts: Vec<ArtifactRef> = record.artifact_refs.clone();

    let bundle = RunEvidenceBundle {
        schema_version: RUN_EVIDENCE_SCHEMA_VERSION,
        run: RunEvidenceRun {
            run_id: manifest.run_id.clone(),
            code_commit: manifest.code_commit.clone(),
            runtime_version: manifest.runtime_version.clone(),
            config_hash: manifest.config_hash.clone(),
            data_fingerprint: manifest.data_fingerprint.clone(),
            input_components: manifest.input_components.clone(),
            clock_start: manifest.clock_start,
            clock_end: manifest.clock_end,
            global_seed: manifest.global_seed,
            determinism_mode: manifest.determinism_mode,
            result_hash: manifest.result_hash.clone(),
            strategy_version: manifest.strategy_version.clone(),
            instrument_spec_version: manifest.instrument_spec_version.clone(),
            model_fingerprint: manifest.model_fingerprint.clone(),
            input_event_hash: manifest.input_event_hash.clone(),
            output_event_hash: manifest.output_event_hash.clone(),
        },
        verification: RunEvidenceVerification {
            // 本地回测产出的证据只到 L1；L3/L4 需要带真实 venue 窗口的 EvidenceBundle。
            capability_level: qx_spec::CapabilityLevel::L1,
            replay_verdict: record.replay_verdict,
            artifact_digests_verified: true,
            verified_artifact_count: artifacts.len() as u64,
        },
        dataset: RunEvidenceDataset {
            kind: summary_str(summary, &["input", "kind"])?,
            dataset_id: summary_str(summary, &["input", "dataset_id"])?,
            version: summary_str(summary, &["input", "dataset_version"])?,
            content_fingerprint: summary_str(summary, &["input", "fingerprint"])?,
            // 合成身份取自 RunManifest（形如 `barframe:<内容哈希>` 或 `dataset-bundle:<指纹>`），
            // 与上面那格内容哈希不是同一个值——对象层会把这一格与 run 块钉成等号。
            composed_fingerprint: manifest.data_fingerprint.clone(),
            row_count: summary_u64(summary, &["bars"])?,
            path: summary_str(summary, &["input", "path"])?,
            quality_report: None,
        },
        identity: RunEvidenceIdentity {
            strategy_id: summary_str(summary, &["strategy_id"])?,
            strategy_version: manifest.strategy_version.clone(),
            instrument: summary_str(summary, &["instrument"])?,
            config_digest: manifest.config_hash.clone(),
        },
        assumptions: assumption_rows(summary)?,
        build: RunEvidenceBuild {
            runtime_version: manifest.runtime_version.clone(),
            code_commit: manifest.code_commit.clone(),
            profile: build_identity::BUILD_PROFILE.to_string(),
            target_triple: build_identity::TARGET_TRIPLE.to_string(),
        },
        unverified: unverified_rows(summary)?,
        recompute: RunEvidenceRecompute {
            steps: vec![
                format!(
                    "按 run.data_fingerprint={} 取回同一份数据集输入（摘要 input.path 那一份）",
                    manifest.data_fingerprint
                ),
                format!(
                    "用 config_hash={} 对应的运行时配置重跑同一档回测入口",
                    manifest.config_hash
                ),
                format!(
                    "比对重跑产出的 result_hash 与 recompute.expected_result_hash={}，两者必须逐字相等",
                    manifest.result_hash
                ),
            ],
            inputs: {
                let mut inputs = vec![
                    manifest_path.clone(),
                    summary_path.display().to_string(),
                    summary_str(summary, &["input", "path"])?,
                ];
                inputs.extend(artifacts.iter().map(|artifact| artifact.path.clone()));
                inputs
            },
            expected_result_hash: manifest.result_hash.clone(),
        },
        operator: std::env::var("QX_OPERATOR").unwrap_or_else(|_| "local".to_string()),
        artifacts,
    };

    let payload = bundle.to_json()?;
    // 走统一规格漏斗再读一次：写侧不许绕过对象校验，schema 版本漂移当场暴露。
    let readback = RunEvidenceBundle::from_json(&payload)?;
    if readback != bundle {
        return Err("运行证据包规范化读回与写入对象不一致".into());
    }
    let path = summary_path.with_file_name(
        summary_path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".summary.json"))
            .map(|stem| format!("{stem}.evidence.json"))
            .ok_or_else(|| format!("回测摘要文件名无效: {}", summary_path.display()))?,
    );
    crate::backtests::write_backtest_artifact(&path, &payload, "RunEvidenceBundle")?;
    Ok(path)
}
