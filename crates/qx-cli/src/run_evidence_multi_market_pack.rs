use crate::*;
use qx_spec::{
    ArtifactRef, CapabilityLevel, EvidenceBundle, ReplayVerdict, RunEvidenceAssumption,
    RunEvidenceBuild, RunEvidenceDataset, RunEvidenceDatasetComponent, RunEvidenceFidelity,
    RunEvidenceIdentity, RunEvidencePayload, RunEvidenceRecompute, RunEvidenceRun,
    RunEvidenceVerification,
};
use std::path::{Path, PathBuf};

pub(crate) fn write_multi_market_run_evidence(artifact_path: &Path) -> Result<PathBuf, String> {
    let artifact_payload = std::fs::read_to_string(artifact_path).map_err(|error| {
        format!(
            "读取多市场运行产物失败 {}: {error}",
            artifact_path.display()
        )
    })?;
    let artifact: serde_json::Value = serde_json::from_str(&artifact_payload)
        .map_err(|error| format!("多市场运行产物 JSON 无效: {error}"))?;
    if artifact.pointer("/spec/markets/0/depth_path").is_some() {
        return super::run_evidence_multi_market_depth::write_multi_market_depth_run_evidence(
            artifact_path,
        );
    }
    let spec: qx_app::MultiMarketBacktestSpec = serde_json::from_value(
        artifact
            .get("spec")
            .cloned()
            .ok_or("多市场运行产物缺少 spec")?,
    )
    .map_err(|error| format!("多市场运行产物的 spec 无效: {error}"))?;
    if spec.markets.len() < 3 {
        return Err("multi-market EvidenceBundle 至少需要三个市场".into());
    }
    if artifact
        .get("replay_verified")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Err("多市场运行产物没有 replay_verified=true，拒绝签发 run EvidenceBundle".into());
    }
    let manifest_path = resolve_artifact_path(
        artifact
            .get("run_manifest_path")
            .and_then(serde_json::Value::as_str)
            .ok_or("多市场运行产物缺少 run_manifest_path")?,
    )?;
    let manifest = qx_core::RunManifest::from_json(
        &std::fs::read_to_string(&manifest_path)
            .map_err(|error| format!("读取多市场 RunManifest 失败: {error}"))?,
    )?;
    if manifest.run_id != spec.run_id {
        return Err("多市场运行产物 spec.run_id 与 RunManifest 不一致".into());
    }
    let events: Vec<qx_core::Event> = serde_json::from_value(
        artifact
            .get("event_log")
            .cloned()
            .ok_or("多市场运行产物缺少 event_log")?,
    )
    .map_err(|error| format!("多市场 event_log 无效: {error}"))?;
    let (replayed_log, replayed_ledger) = qx_core::ReplayVerifier::replay_owned(events)
        .map_err(|error| format!("多市场运行产物重放失败: {error}"))?;
    replayed_log
        .validate()
        .map_err(|error| format!("多市场重放 EventLog 校验失败: {error}"))?;
    let replayed_entries = serde_json::to_value(replayed_ledger.entries())
        .map_err(|error| format!("序列化重放账簿失败: {error}"))?;
    if artifact.get("ledger_entries") != Some(&replayed_entries) {
        return Err("多市场运行产物账簿与事件流重放结果不一致".into());
    }
    let replayed_result_hash = format!("{:016x}", replayed_log.digest());
    if replayed_result_hash != manifest.result_hash
        || artifact
            .pointer("/outcome/shared_result_hash")
            .and_then(serde_json::Value::as_str)
            != Some(manifest.result_hash.as_str())
    {
        return Err("多市场运行产物 replay hash、outcome 与 RunManifest 不一致".into());
    }
    let equity_path = resolve_artifact_path(
        artifact
            .get("equity_path")
            .and_then(serde_json::Value::as_str)
            .ok_or("多市场运行产物缺少 equity_path")?,
    )?;
    let fills_path = resolve_artifact_path(
        artifact
            .get("fills_path")
            .and_then(serde_json::Value::as_str)
            .ok_or("多市场运行产物缺少 fills_path")?,
    )?;
    let mut components = Vec::with_capacity(spec.markets.len());
    let mut identities = Vec::with_capacity(spec.markets.len());
    let mut input_paths = Vec::with_capacity(spec.markets.len());
    let mut row_count = 0_u64;
    for market in &spec.markets {
        let role = format!("market:{}", market.instrument);
        let market_identity = artifact
            .get("market_data")
            .and_then(serde_json::Value::as_array)
            .and_then(|items| {
                items.iter().find(|item| {
                    item.get("instrument").and_then(serde_json::Value::as_str)
                        == Some(market.instrument.as_str())
                })
            })
            .ok_or_else(|| format!("多市场运行产物缺少 {role} 的 market_data 身份"))?;
        let path_text = market_identity
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("多市场运行产物缺少 {role} 的输入路径"))?;
        let path = Path::new(path_text)
            .canonicalize()
            .map_err(|error| format!("解析 {role} BarFrame 路径失败: {error}"))?;
        let frame = read_bar_frame_for_backtest(&path)?;
        if frame.instrument.to_string() != market.instrument {
            return Err(format!("{role} BarFrame 标的与 spec 不一致"));
        }
        let fingerprint = qx_data::fingerprint_bar_frame(&frame)
            .map_err(|error| format!("重算 {role} BarFrame 指纹失败: {error}"))?;
        if market_identity
            .get("content_hash")
            .and_then(serde_json::Value::as_str)
            != Some(fingerprint.as_str())
        {
            return Err(format!("{role} BarFrame 指纹与运行产物声明不一致"));
        }
        let rows = u64::try_from(frame.ts.len()).map_err(|_| "BarFrame 行数超出 u64 范围")?;
        if market_identity
            .get("row_count")
            .and_then(serde_json::Value::as_u64)
            != Some(rows)
        {
            return Err(format!("{role} BarFrame 行数与运行产物声明不一致"));
        }
        row_count = row_count
            .checked_add(rows)
            .ok_or("多市场 BarFrame 行数总和溢出")?;
        identities.push(format!("{role}={fingerprint}"));
        input_paths.push(path.to_string_lossy().to_string());
        let quality = artifact
            .pointer(&format!("/dataset_quality/{}", market.instrument))
            .filter(|value| !value.is_null());
        let quality_manifest_path = quality
            .and_then(|value| value.get("manifest_path"))
            .and_then(serde_json::Value::as_str)
            .map(|value| {
                Path::new(value).canonicalize().map_err(|error| {
                    format!("解析 {} 质量清单路径失败: {error}", market.instrument)
                })
            })
            .transpose()?;
        let quality_report = quality
            .and_then(|value| value.get("quality_report"))
            .map(
                |value| -> Result<qx_spec::RunEvidenceQualityReport, String> {
                    let report: qx_data::DatasetQualityReport =
                        serde_json::from_value(value.clone()).map_err(|error| {
                            format!("{} 质量报告无效: {error}", market.instrument)
                        })?;
                    Ok(qx_spec::RunEvidenceQualityReport {
                        duplicate_rows: report.duplicate_rows,
                        out_of_order_rows: report.out_of_order_rows,
                        missing_intervals: report.missing_intervals,
                        interval_ms: report.interval_ms,
                        timezone: report.timezone,
                        corporate_action_coverage: report.corporate_action_coverage,
                        usable_tiers: report
                            .usable_tiers
                            .into_iter()
                            .map(|tier| tier.as_str().to_string())
                            .collect(),
                    })
                },
            )
            .transpose()?;
        let quality_manifest_digest = quality_manifest_path
            .as_ref()
            .map(qx_strategy::file_digest::sha256_file_hex)
            .transpose()?;
        components.push(RunEvidenceDatasetComponent {
            role,
            dataset_id: format!("market:{}", market.instrument),
            version: qx_data::BARFRAME_DATASET_VERSION.to_string(),
            content_fingerprint: fingerprint,
            row_count: rows,
            path: path.to_string_lossy().to_string(),
            manifest_fingerprint: quality
                .and_then(|value| value.get("manifest_fingerprint"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            quality_report,
            quality_manifest_path: quality_manifest_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            quality_manifest_sha256: quality_manifest_digest,
        });
        if let Some(path) = quality_manifest_path {
            input_paths.push(path.to_string_lossy().to_string());
        }
    }
    let composed_content_fingerprint = qx_strategy::sha256_hex(identities.join("|").as_bytes());
    let input_paths_joined = input_paths.join("|");
    let mut artifact_refs = vec![
        ("run_manifest", manifest_path.clone()),
        (
            "summary",
            artifact_path
                .canonicalize()
                .map_err(|error| error.to_string())?,
        ),
        ("equity", equity_path),
        ("fills", fills_path),
    ]
    .into_iter()
    .map(|(name, path)| {
        Ok(ArtifactRef {
            name: name.to_string(),
            path: path.to_string_lossy().to_string(),
            digest: qx_strategy::file_digest::sha256_file_hex(&path)?,
        })
    })
    .collect::<Result<Vec<_>, String>>()?;
    for component in &components {
        if let (Some(path), Some(digest)) = (
            component.quality_manifest_path.as_ref(),
            component.quality_manifest_sha256.as_ref(),
        ) {
            artifact_refs.push(ArtifactRef {
                name: format!("dataset-manifest:{}", component.role),
                path: path.clone(),
                digest: digest.clone(),
            });
        }
    }
    let strategy_ids = spec
        .markets
        .iter()
        .flat_map(|market| &market.strategies)
        .map(|slot| slot.strategy.strategy_id.as_str())
        .collect::<Vec<_>>();
    let strategy_id = format!("multi-market:{}", strategy_ids.join("+"));
    let has_quality_reports = components
        .iter()
        .any(|component| component.quality_report.is_some());
    let verified_artifact_count =
        u64::try_from(artifact_refs.len()).map_err(|_| "多市场证据产物数量超出 u64")?;
    let assumptions = vec![
        RunEvidenceAssumption {
            name: "matching_kernel".into(),
            value: "PortfolioBarEngine/v1".into(),
            source: "qx-app run_multi_market_backtest".into(),
        },
        RunEvidenceAssumption {
            name: "initial_cash_raw".into(),
            value: spec.initial_cash_raw.to_string(),
            source: "MultiMarketBacktestSpec.initial_cash_raw".into(),
        },
        RunEvidenceAssumption {
            name: "risk_rule_set".into(),
            value: "conservative_default".into(),
            source: "PortfolioBarSessionConfig::risk".into(),
        },
        RunEvidenceAssumption {
            name: "market_fee_and_contract_specs".into(),
            value: serde_json::to_string(&spec.markets)
                .map_err(|error| format!("序列化多市场规格假设失败: {error}"))?,
            source: "MultiMarketBacktestSpec.markets".into(),
        },
    ];
    let run = RunEvidenceRun {
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
    };
    let run_evidence = RunEvidencePayload {
        run,
        artifacts: artifact_refs,
        dataset: RunEvidenceDataset {
            kind: "multi-market-barframe".into(),
            dataset_id: format!(
                "multi-market:{}",
                spec.markets
                    .iter()
                    .map(|market| market.instrument.as_str())
                    .collect::<Vec<_>>()
                    .join("+")
            ),
            version: "multi-market-barframe-v1".into(),
            content_fingerprint: composed_content_fingerprint,
            composed_fingerprint: manifest.data_fingerprint.clone(),
            row_count,
            path: input_paths_joined.clone(),
            components,
            ..RunEvidenceDataset::default()
        },
        report_analysis: None,
        identity: RunEvidenceIdentity {
            strategy_id,
            strategy_version: manifest.strategy_version.clone(),
            instrument: "multi-market-portfolio".into(),
            config_digest: manifest.config_hash.clone(),
        },
        assumptions,
        fidelity: Some(RunEvidenceFidelity {
            source_artifact: "run".into(),
            observed: vec![
                "/dataset/components".into(),
                "/dataset/composed_fingerprint".into(),
                "/run/input_event_hash".into(),
            ],
            configured: vec![
                "/identity/config_digest".into(),
                "/assumptions/1".into(),
                "/assumptions/2".into(),
                "/assumptions/3".into(),
            ],
            estimated: vec!["/assumptions/0".into()],
            computed: vec![
                "/run/result_hash".into(),
                "/verification/replay_verdict".into(),
            ],
            absent: if has_quality_reports {
                Vec::new()
            } else {
                vec!["/dataset/quality_report".into()]
            },
        }),
        build: RunEvidenceBuild {
            runtime_version: manifest.runtime_version.clone(),
            code_commit: manifest.code_commit.clone(),
            profile: "unknown".into(),
            target_triple: "unknown".into(),
        },
        verification: RunEvidenceVerification {
            capability_level: CapabilityLevel::L1,
            replay_verdict: ReplayVerdict::Verified,
            artifact_digests_verified: true,
            verified_artifact_count,
        },
        unverified: vec![
            "本次运行未接触外部 venue；证据只覆盖本地多市场 Bar 回测".into(),
            if has_quality_reports {
                "质量报告只核验清单声明及 BarFrame 可重算字段；不证明数据源权威性或公司行为完整性"
                    .into()
            } else {
                "输入数据没有附带质量报告；本包核验内容指纹与行数，不为数据质量背书".into()
            },
            "打包时已从 event_log 重放账簿并核对 RunManifest；第三方独立构建复算仍需匹配二进制"
                .into(),
            "RunManifest 没有记录原始构建 profile 与 target triple；此证据包将两项标为 unknown"
                .into(),
            "若本次绑定了外部策略，其可执行文件尚未纳入本包的重建输入".into(),
        ],
        recompute: RunEvidenceRecompute {
            steps: vec![
                "逐市场重读 BarFrame，复核 instrument、行数、数据指纹与 RunManifest 输入组件"
                    .into(),
                "使用匹配 code_commit 的构建与隔离输出目录重跑 multi-market-backtest".into(),
                "逐字段比较重跑 RunManifest，并比对 result_hash".into(),
            ],
            inputs: std::iter::once(manifest_path.to_string_lossy().to_string())
                .chain(std::iter::once(artifact_path.to_string_lossy().to_string()))
                .chain(input_paths)
                .collect(),
            expected_result_hash: manifest.result_hash.clone(),
        },
        operator: std::env::var("QX_OPERATOR").unwrap_or_else(|_| "local".into()),
    };
    let bundle = EvidenceBundle::run(run_evidence);
    let payload = qx_app::artifact_store::serialize_verified_evidence_bundle(&bundle, |_, _| {
        "多市场 EvidenceBundle 规范化读回与写入对象不一致".into()
    })?;
    let output_path = artifact_path.with_file_name(format!(
        "{}.evidence.json",
        artifact_path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".multi-market.json"))
            .ok_or_else(|| format!("多市场产物文件名无效: {}", artifact_path.display()))?
    ));
    crate::backtests::write_backtest_artifact(&output_path, &payload, "EvidenceBundle")?;
    Ok(output_path)
}

fn resolve_artifact_path(reference: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(reference);
    if !path.is_absolute() {
        return Err(format!("多市场运行产物引用必须是绝对路径: {reference}"));
    }
    path.canonicalize()
        .map_err(|error| format!("解析多市场产物引用失败 {reference}: {error}"))
}
