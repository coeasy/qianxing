use crate::*;
use qx_spec::{
    ArtifactRef, CapabilityLevel, EvidenceBundle, ReplayVerdict, RunEvidenceAssumption,
    RunEvidenceBuild, RunEvidenceDataset, RunEvidenceDatasetComponent, RunEvidenceFidelity,
    RunEvidenceIdentity, RunEvidencePayload, RunEvidenceRecompute, RunEvidenceRun,
    RunEvidenceVerification,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(crate) fn write_multi_market_depth_run_evidence(path: &Path) -> Result<PathBuf, String> {
    let artifact: serde_json::Value = serde_json::from_slice(
        &std::fs::read(path).map_err(|error| format!("读取多市场深度产物失败: {error}"))?,
    )
    .map_err(|error| format!("多市场深度产物 JSON 无效: {error}"))?;
    let spec: qx_app::MultiMarketDepthBacktestSpec = serde_json::from_value(
        artifact
            .get("spec")
            .cloned()
            .ok_or("多市场深度产物缺少 spec")?,
    )
    .map_err(|error| format!("多市场深度 spec 无效: {error}"))?;
    spec.validate().map_err(|error| error.to_string())?;
    let outcome: qx_app::MultiMarketDepthBacktestOutcome = serde_json::from_value(
        artifact
            .get("outcome")
            .cloned()
            .ok_or("多市场深度产物缺少 outcome")?,
    )
    .map_err(|error| format!("多市场深度 outcome 无效: {error}"))?;
    if !outcome.execution_models.is_empty() {
        let declared = spec
            .markets
            .iter()
            .map(|market| {
                Ok((
                    market.instrument.clone(),
                    market
                        .execution_model()
                        .map_err(|error| error.to_string())?
                        .audit_descriptor(),
                ))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        if outcome.execution_models != declared {
            return Err("多市场深度 execution_models 与 spec 声明的生效模型不一致".into());
        }
    }
    if artifact
        .get("replay_verified")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return Err("多市场深度产物未声明 replay_verified=true".into());
    }
    let manifest_path = absolute_artifact_path(
        artifact
            .pointer("/run_manifest_path")
            .and_then(serde_json::Value::as_str)
            .ok_or("多市场深度产物缺少 RunManifest 路径")?,
    )?;
    let manifest = qx_core::RunManifest::from_json(
        &std::fs::read_to_string(&manifest_path)
            .map_err(|error| format!("读取多市场深度 RunManifest 失败: {error}"))?,
    )?;
    if manifest.run_id != spec.run_id {
        return Err("多市场深度 RunManifest run_id 不一致".into());
    }
    let events: Vec<qx_core::Event> = serde_json::from_value(
        artifact
            .get("event_log")
            .cloned()
            .ok_or("多市场深度产物缺少 event_log")?,
    )
    .map_err(|error| format!("多市场深度 event_log 无效: {error}"))?;
    let (log, ledger) = qx_core::ReplayVerifier::replay_owned(events)
        .map_err(|error| format!("多市场深度事件重放失败: {error}"))?;
    log.validate()
        .map_err(|error| format!("多市场深度重放日志无效: {error}"))?;
    if artifact.get("ledger_entries")
        != Some(&serde_json::to_value(ledger.entries()).map_err(|error| error.to_string())?)
        || format!("{:016x}", log.digest()) != manifest.output_event_hash
        || format!("{:016x}", log.digest()) != manifest.result_hash
        || artifact
            .pointer("/outcome/shared_result_hash")
            .and_then(serde_json::Value::as_str)
            != Some(manifest.result_hash.as_str())
    {
        return Err("多市场深度重放账簿/事件摘要与 RunManifest 不一致".into());
    }
    let equity_path = absolute_artifact_path(
        artifact
            .pointer("/equity_path")
            .and_then(serde_json::Value::as_str)
            .ok_or("缺少 equity sidecar")?,
    )?;
    let fills_path = absolute_artifact_path(
        artifact
            .pointer("/fills_path")
            .and_then(serde_json::Value::as_str)
            .ok_or("缺少 fills sidecar")?,
    )?;
    let equity: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&equity_path)
            .map_err(|error| format!("读取多市场深度权益 sidecar 失败: {error}"))?,
    )
    .map_err(|error| format!("多市场深度权益 sidecar JSON 无效: {error}"))?;
    if artifact.get("equity") != Some(&equity) {
        return Err("多市场深度权益 sidecar 与摘要内 equity 不一致".into());
    }
    let fills: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&fills_path)
            .map_err(|error| format!("读取多市场深度成交 sidecar 失败: {error}"))?,
    )
    .map_err(|error| format!("多市场深度成交 sidecar JSON 无效: {error}"))?;
    let fill_count = fills
        .as_array()
        .and_then(|items| u64::try_from(items.len()).ok())
        .ok_or("多市场深度 fills sidecar 必须是可计数数组")?;
    if artifact
        .pointer("/outcome/successful_fills")
        .and_then(serde_json::Value::as_u64)
        != Some(fill_count)
    {
        return Err("多市场深度成交 sidecar 数量与 outcome.successful_fills 不一致".into());
    }
    let market_data = artifact
        .get("market_data")
        .and_then(serde_json::Value::as_array)
        .ok_or("多市场深度产物缺少 market_data")?;
    let mut components = Vec::with_capacity(spec.markets.len());
    let mut ids = Vec::with_capacity(spec.markets.len());
    let mut paths = Vec::with_capacity(spec.markets.len());
    let mut quality_paths = Vec::new();
    let mut rows = 0_u64;
    let mut data_hasher = qx_core::Fnv1a::new();
    let mut manifest_components = BTreeMap::new();
    for market in &spec.markets {
        let role = format!("market:{}", market.instrument);
        let identity = market_data
            .iter()
            .find(|item| {
                item.get("instrument").and_then(serde_json::Value::as_str)
                    == Some(market.instrument.as_str())
            })
            .ok_or_else(|| format!("缺少 {role} 身份"))?;
        let data_tier = identity
            .get("data_tier")
            .and_then(serde_json::Value::as_str)
            .ok_or("缺少深度数据档位")?;
        if !matches!(data_tier, "l1" | "l2") {
            return Err(format!("{role} 深度数据档位声明非法"));
        }
        let source = identity
            .get("source")
            .and_then(serde_json::Value::as_str)
            .ok_or("缺少深度来源")?;
        let path_text = identity
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or("缺少深度输入路径")?;
        let input = Path::new(path_text)
            .canonicalize()
            .map_err(|error| format!("解析 {role} 输入失败: {error}"))?;
        let frame = read_depth_frame_for_backtest(&input)?;
        if frame.instrument.to_string() != market.instrument {
            return Err(format!("{role} 深度标的与 spec 不匹配"));
        }
        let hash = format!("{:016x}", frame.input_hash());
        if identity
            .get("content_hash")
            .and_then(serde_json::Value::as_str)
            != Some(hash.as_str())
            || frame.source != source
        {
            return Err(format!("{role} 深度内容身份不匹配"));
        }
        let count = u64::try_from(frame.snapshots.len()).map_err(|_| "深度快照数超出 u64")?;
        if identity
            .get("snapshot_count")
            .and_then(serde_json::Value::as_u64)
            != Some(count)
        {
            return Err(format!("{role} 快照数不匹配"));
        }
        if data_tier == "l1"
            && frame
                .snapshots
                .iter()
                .any(|snapshot| snapshot.bids.len() != 1 || snapshot.asks.len() != 1)
        {
            return Err(format!("{role} 声明 L1 但包含多档深度"));
        }
        rows = rows.checked_add(count).ok_or("多市场深度快照总数溢出")?;
        let fingerprint = format!("depth:{data_tier}:{source}:{hash}");
        data_hasher.write_text(&market.instrument);
        data_hasher.write_text(data_tier);
        data_hasher.write_text(source);
        data_hasher.write_u64(frame.input_hash());
        manifest_components.insert(
            format!("depthframe:{}", market.instrument),
            fingerprint.clone(),
        );
        let quality_value = market_data
            .iter()
            .find(|item| {
                item.get("instrument").and_then(serde_json::Value::as_str)
                    == Some(market.instrument.as_str())
            })
            .and_then(|item| item.get("dataset_quality"))
            .filter(|value| !value.is_null());
        let (manifest_fingerprint, quality_report, quality_manifest_path, quality_manifest_sha256) =
            if let Some(value) = quality_value {
                let manifest_path_text = value
                    .get("manifest_path")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| format!("{role} 质量清单缺少路径"))?;
                let manifest_path = Path::new(manifest_path_text)
                    .canonicalize()
                    .map_err(|error| format!("解析 {role} 质量清单失败: {error}"))?;
                let bytes = std::fs::read(&manifest_path).map_err(|error| error.to_string())?;
                let parsed = qx_data::DatasetManifestV2::from_json(
                    std::str::from_utf8(&bytes).map_err(|error| error.to_string())?,
                )?;
                let mf = parsed.fingerprint()?;
                let sha = qx_data::sha256_hex_bytes(&bytes);
                if value
                    .get("manifest_fingerprint")
                    .and_then(serde_json::Value::as_str)
                    != Some(mf.as_str())
                    || value
                        .get("manifest_sha256")
                        .and_then(serde_json::Value::as_str)
                        != Some(sha.as_str())
                    || parsed.instrument != market.instrument
                    || parsed.content_hash != qx_strategy::file_digest::sha256_file_hex(&input)?
                    || parsed.dataset.fingerprint != format!("{:016x}", frame.input_hash())
                {
                    return Err(format!("{role} 质量清单与输入不一致"));
                }
                data_hasher.write_text(&mf);
                manifest_components
                    .insert(format!("dataset-quality:{}", market.instrument), mf.clone());
                let report = parsed.quality_report;
                let evidence_report = qx_spec::RunEvidenceQualityReport {
                    duplicate_rows: report.duplicate_rows,
                    out_of_order_rows: report.out_of_order_rows,
                    missing_intervals: report.missing_intervals,
                    interval_ms: report.interval_ms,
                    timezone: report.timezone,
                    corporate_action_coverage: report.corporate_action_coverage,
                    usable_tiers: report.usable_tiers,
                };
                quality_paths.push(manifest_path.to_string_lossy().to_string());
                (
                    Some(mf),
                    Some(evidence_report),
                    Some(manifest_path.to_string_lossy().to_string()),
                    Some(sha),
                )
            } else {
                (None, None, None, None)
            };
        ids.push(format!("{role}={fingerprint}"));
        paths.push(input.to_string_lossy().to_string());
        components.push(RunEvidenceDatasetComponent {
            role,
            dataset_id: format!("depth:{}", market.instrument),
            version: "depthframe-v1".into(),
            content_fingerprint: fingerprint,
            row_count: count,
            path: input.to_string_lossy().to_string(),
            manifest_fingerprint,
            quality_report,
            quality_manifest_path,
            quality_manifest_sha256,
        });
    }
    let composed_fingerprint = format!("{:016x}", data_hasher.finish());
    if manifest.data_fingerprint != composed_fingerprint
        || manifest.input_event_hash != composed_fingerprint
        || manifest.input_components != manifest_components
    {
        return Err("多市场深度输入组件与 RunManifest 数据身份不一致".into());
    }
    let mut identity_spec = spec.clone();
    identity_spec.output_dir.clear();
    for market in &mut identity_spec.markets {
        market.depth_path.clear();
        market.dataset_manifest_path = None;
    }
    let identity_bytes = serde_json::to_vec(&identity_spec).map_err(|error| error.to_string())?;
    if manifest.config_hash != qx_strategy::sha256_hex(&identity_bytes) {
        return Err("多市场深度配置摘要与 RunManifest 不一致".into());
    }
    let mut spec_hasher = qx_core::Fnv1a::new();
    spec_hasher.write_text(&identity_spec.to_json().map_err(|error| error.to_string())?);
    let mut identities = artifact
        .get("strategy_identities")
        .cloned()
        .map(serde_json::from_value::<Vec<qx_app::StrategyArtifactIdentity>>)
        .transpose()
        .map_err(|error| format!("多市场深度策略身份无效: {error}"))?
        .unwrap_or_default();
    identities.sort_by(|left, right| left.strategy_id.cmp(&right.strategy_id));
    for identity in &identities {
        spec_hasher.write_text(&identity.strategy_id);
        spec_hasher.write_text(&identity.strategy_version);
        spec_hasher.write_text(&identity.source_sha256);
    }
    if manifest.strategy_version != format!("multi-market-depth:{:016x}", spec_hasher.finish()) {
        return Err("多市场深度策略规格摘要与 RunManifest 不一致".into());
    }
    let dataset_fingerprint = qx_strategy::sha256_hex(ids.join("|").as_bytes());
    let summary_path = path.canonicalize().map_err(|error| error.to_string())?;
    let mut artifact_refs = [
        ("run_manifest", manifest_path.clone()),
        ("summary", summary_path.clone()),
        ("equity", equity_path),
        ("fills", fills_path),
    ]
    .into_iter()
    .map(|(name, path)| {
        Ok(ArtifactRef {
            name: name.into(),
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
        .map(|market| market.strategy.strategy_id.as_str())
        .collect::<Vec<_>>();
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
    let mut assumptions = vec![
        RunEvidenceAssumption {
            name: "matching_kernel".into(),
            value: "OrderBookPortfolioBacktestEngine/v1".into(),
            source: "qx-app run_multi_market_depth_backtest".into(),
        },
        RunEvidenceAssumption {
            name: "market_execution_specs".into(),
            value: serde_json::to_string(&spec.markets).map_err(|error| error.to_string())?,
            source: "MultiMarketDepthBacktestSpec.markets".into(),
        },
    ];
    for (instrument, descriptor) in &outcome.execution_models {
        assumptions.push(RunEvidenceAssumption {
            name: format!("orderbook_execution_model:{instrument}"),
            value: serde_json::to_string(descriptor).map_err(|error| error.to_string())?,
            source: "kernel-self-described".into(),
        });
        assumptions.push(RunEvidenceAssumption {
            name: format!("queue_position_model:{instrument}"),
            value: serde_json::to_string(&descriptor.queue_position)
                .map_err(|error| error.to_string())?,
            source: "kernel-self-described".into(),
        });
    }
    let mut estimated_paths = vec!["/assumptions/0".to_string()];
    for market_index in 0..outcome.execution_models.len() {
        let assumption_index = 2 + market_index * 2;
        estimated_paths.push(format!("/assumptions/{assumption_index}"));
        estimated_paths.push(format!("/assumptions/{}", assumption_index + 1));
    }
    let has_quality_reports = components
        .iter()
        .any(|component| component.quality_report.is_some());
    let verified_artifact_count =
        u64::try_from(artifact_refs.len()).map_err(|_| "多市场深度 EvidenceBundle 产物数量溢出")?;
    let bundle = EvidenceBundle::run(RunEvidencePayload {
        run,
        artifacts: artifact_refs,
        dataset: RunEvidenceDataset {
            kind: "multi-market-depthframe".into(),
            dataset_id: format!(
                "multi-market-depth:{}",
                spec.markets
                    .iter()
                    .map(|market| market.instrument.as_str())
                    .collect::<Vec<_>>()
                    .join("+")
            ),
            version: "multi-market-depthframe-v1".into(),
            content_fingerprint: dataset_fingerprint,
            composed_fingerprint: manifest.data_fingerprint.clone(),
            row_count: rows,
            path: paths.join("|"),
            components,
            ..RunEvidenceDataset::default()
        },
        report_analysis: None,
        identity: RunEvidenceIdentity {
            strategy_id: format!("multi-market-depth:{}", strategy_ids.join("+")),
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
            configured: vec!["/identity/config_digest".into(), "/assumptions/1".into()],
            estimated: estimated_paths,
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
            "仅覆盖本地多市场 L1/L2 深度回测，不代表真实 venue 成交".into(),
            if has_quality_reports {
                "深度质量报告已与输入帧绑定，并纳入运行指纹".into()
            } else {
                "输入未附数据质量报告".into()
            },
            "第三方策略二进制及匹配构建未纳入独立复算".into(),
            "构建 profile/target 未记录，标记 unknown".into(),
        ],
        recompute: RunEvidenceRecompute {
            steps: vec![
                "重算每个 DepthFrame 的标的、快照数、来源和内容身份".into(),
                "以匹配 code_commit 的构建和隔离输出目录重跑多市场深度回测".into(),
                "逐字段比较 RunManifest".into(),
            ],
            inputs: std::iter::once(manifest_path.to_string_lossy().to_string())
                .chain(std::iter::once(summary_path.to_string_lossy().to_string()))
                .chain(paths)
                .chain(quality_paths)
                .collect(),
            expected_result_hash: manifest.result_hash.clone(),
        },
        operator: std::env::var("QX_OPERATOR").unwrap_or_else(|_| "local".into()),
    });
    let json = qx_app::artifact_store::serialize_verified_evidence_bundle(&bundle, |_, _| {
        "多市场深度 EvidenceBundle 读回不一致".into()
    })?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".multi-market-depth.json"))
        .ok_or("多市场深度产物文件名无效")?;
    let output = path.with_file_name(format!("{name}.evidence.json"));
    crate::backtests::write_backtest_artifact(&output, &json, "EvidenceBundle")?;
    Ok(output)
}

fn absolute_artifact_path(text: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(text);
    if !path.is_absolute() {
        return Err(format!("多市场深度产物引用必须是绝对路径: {text}"));
    }
    path.canonicalize()
        .map_err(|error| format!("解析多市场深度产物路径失败: {error}"))
}
