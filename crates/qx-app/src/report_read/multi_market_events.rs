//! Multi-market EventLog report readers and replay-verified page assembly.

use super::artifact_views::malformed_event_log;
use super::*;

/// Stream and replay a shared-account EventLog without materializing its JSON array.
/// The expected event count, digest and ledger are read from already-rooted run artifacts;
/// the requested page is the only event-row data retained after replay.
pub fn read_multi_market_event_report_v1_with_reader(
    reference: &str,
    expected_count: u64,
    expected_digest: &str,
    expected_ledger_entries: &Value,
    strategy_markets: &BTreeMap<String, String>,
    requested_page: Option<(u64, usize)>,
    reader: &dyn ArtifactReader,
) -> Result<VerifiedMultiMarketEventReportV1, AppError> {
    if expected_count > MAX_ARTIFACT_ROWS {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            "多市场摘要声明的事件数超过 500 万行上限",
        ));
    }
    let expected_count_usize = usize::try_from(expected_count)
        .map_err(|_| malformed_event_log("多市场摘要事件数超出本机索引范围"))?;
    let expected_digest = u64::from_str_radix(expected_digest, 16)
        .map_err(|_| malformed_event_log("RunManifest.output_event_hash 不是十六进制摘要"))?;
    if let Some((_, limit)) = requested_page {
        if limit == 0 || limit > MAX_EVENT_REPORT_PAGE_ROWS {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!("事件报告分页 limit 必须在 1 到 {MAX_EVENT_REPORT_PAGE_ROWS} 之间"),
            ));
        }
    }
    let page = requested_page.unwrap_or((
        expected_count.saturating_sub(MAX_EVENT_REPORT_PAGE_ROWS as u64),
        MAX_EVENT_REPORT_PAGE_ROWS,
    ));
    let offset = page.0.min(expected_count);
    let end = offset.saturating_add(page.1 as u64).min(expected_count);
    let mut rows = Vec::with_capacity(usize::try_from(end - offset).unwrap_or(0));
    let mut fill_count = 0_u64;
    let mut fill_digest = qx_strategy::file_digest::Sha256Accumulator::new();
    fill_digest.update(b"qx-event-fill-sequence-v1\0");
    let mut trace_bytes_digest = qx_strategy::file_digest::Sha256Accumulator::new();
    trace_bytes_digest.update(b"qx-multi-market-event-trace-bytes-v1\0");
    let mut replay = qx_core::StreamingReplayVerifier::new(expected_count_usize);
    let mut artifact = reader.open(reference)?;
    if artifact.byte_len > MAX_ARTIFACT_BYTES {
        return Err(artifact_too_large(reference, MAX_ARTIFACT_BYTES));
    }
    let mut bytes_seen = 0_u64;
    let mut total = 0_u64;
    while let Some((line, line_bytes)) = read_bounded_line(artifact.reader.as_mut(), reference)? {
        bytes_seen = bytes_seen.saturating_add(line_bytes as u64);
        trace_bytes_digest.update(line.as_bytes());
        if bytes_seen > MAX_ARTIFACT_BYTES {
            return Err(artifact_too_large(reference, MAX_ARTIFACT_BYTES));
        }
        if line.trim().is_empty() {
            continue;
        }
        total = total.saturating_add(1);
        if total > MAX_ARTIFACT_ROWS {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                "多市场事件轨迹超过 500 万行上限",
            ));
        }
        let event: qx_core::Event = serde_json::from_str(line.trim_end()).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("多市场事件轨迹第 {total} 行格式损坏: {error}"),
            )
        })?;
        if let qx_core::EventKind::Filled { fill } = &event.kind {
            let identity = event_fill_identity(fill);
            update_fill_sequence_digest(&mut fill_digest, &identity)?;
            fill_count = fill_count
                .checked_add(1)
                .ok_or_else(|| malformed_event_log("多市场成交事件计数溢出"))?;
        }
        let event_index = total - 1;
        if event_index >= offset && event_index < end {
            let event_value = serde_json::to_value(&event).map_err(|error| {
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!("多市场事件行投影失败: {error}"),
                )
            })?;
            rows.push(event_value);
        }
        replay.append(event).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("多市场事件轨迹重放失败: {error}"),
            )
        })?;
    }
    let replayed = replay.finish().map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("多市场事件轨迹重放失败: {error}"),
        )
    })?;
    if total != expected_count {
        return Err(malformed_event_log(
            "多市场事件轨迹行数与产物 event_count 不一致",
        ));
    }
    if replayed.log_digest != expected_digest {
        return Err(malformed_event_log(
            "多市场事件轨迹摘要与 RunManifest.output_event_hash 不一致",
        ));
    }
    let replayed_ledger_entries =
        serde_json::to_value(replayed.ledger.entries()).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("多市场重放账簿序列化失败: {error}"),
            )
        })?;
    if replayed_ledger_entries != *expected_ledger_entries {
        return Err(malformed_event_log(
            "多市场事件轨迹重放账簿与产物 ledger_entries 不一致",
        ));
    }
    let unresolved_page_order_ids = rows
        .iter()
        .filter(|event| {
            event_market_instrument(event, strategy_markets, &BTreeMap::new()).is_none()
        })
        .filter_map(event_order_id)
        .collect::<BTreeSet<_>>();
    let order_markets = if unresolved_page_order_ids.is_empty() {
        BTreeMap::new()
    } else {
        read_multi_market_order_market_index_v1(
            reference,
            &trace_bytes_digest.finish_hex(),
            &unresolved_page_order_ids,
            strategy_markets,
            reader,
        )?
    };
    Ok(VerifiedMultiMarketEventReportV1 {
        total,
        offset,
        truncated: rows.len() < total as usize,
        rows,
        fill_count,
        fill_sequence_sha256: fill_digest.finish_hex(),
        order_markets,
        ledger: replayed.ledger,
    })
}

/// Read a bounded multi-market EventLog page through the caller's artifact port and return the
/// language-neutral JSON core. The complete trace is replayed and checked before any page is
/// returned; only the requested rows are retained in the result.
pub fn read_multi_market_event_report_page_v1_with_reader(
    request: &MultiMarketEventReportReadRequestV1,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<MultiMarketEventReportPageV1, AppError> {
    read_multi_market_event_report_read_model_v1_with_reader(request, context, reader)
        .map(|model| model.page)
}

/// Verify a multi-market report once and retain the bounded page plus verified inputs used by
/// an application adapter to add its own authorized data/context projection.
pub fn read_multi_market_event_report_read_model_v1_with_reader(
    request: &MultiMarketEventReportReadRequestV1,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<MultiMarketEventReportReadModelV1, AppError> {
    context.require(CallerCapability::Research, "多市场事件报告读取")?;
    if request.schema_version != MULTI_MARKET_EVENT_REPORT_SCHEMA_VERSION_V1 {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "多市场事件报告读取请求 schema_version 不受支持",
        ));
    }
    let outcome = &request.outcome;
    if outcome.run_id.trim().is_empty() || outcome.artifact_path.trim().is_empty() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "多市场事件报告读取请求缺少有效 outcome 身份",
        ));
    }
    let artifact_text = read_text_bounded(
        reader,
        &outcome.artifact_path,
        MAX_ARTIFACT_BYTES,
        "多市场回测产物",
    )?;
    let artifact: Value = serde_json::from_str(&artifact_text).map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场回测产物 JSON 无效: {error}"),
        )
    })?;
    let artifact_version = artifact.get("schema_version").and_then(Value::as_u64);
    if !matches!(artifact_version, Some(1 | 2)) {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场回测产物 schema_version 不受支持",
        ));
    }
    let embedded_outcome: MultiMarketBacktestOutcome = serde_json::from_value(
        artifact
            .get("outcome")
            .cloned()
            .ok_or_else(|| AppError::new(AppErrorCategory::Conflict, "多市场产物缺少 outcome"))?,
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场产物 outcome 无效: {error}"),
        )
    })?;
    if embedded_outcome != *outcome {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "请求 outcome 与多市场回测产物身份不一致",
        ));
    }
    let spec: MultiMarketBacktestSpec = serde_json::from_value(
        artifact
            .get("spec")
            .cloned()
            .ok_or_else(|| AppError::new(AppErrorCategory::Conflict, "多市场产物缺少 spec"))?,
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场产物 spec 无效: {error}"),
        )
    })?;
    spec.validate()?;
    let strategies_total = spec.markets.iter().try_fold(0_usize, |total, market| {
        total
            .checked_add(market.strategies.len())
            .ok_or_else(|| AppError::new(AppErrorCategory::Conflict, "多市场策略总数溢出"))
    })?;
    if spec.run_id != outcome.run_id
        || spec.account_id != outcome.account_id
        || spec.markets.len() != outcome.markets_total
        || strategies_total != outcome.strategies_total
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场 spec 与 outcome 摘要不一致",
        ));
    }
    let strategy_identities: Vec<StrategyArtifactIdentity> = serde_json::from_value(
        artifact
            .get("strategy_identities")
            .cloned()
            .ok_or_else(|| {
                AppError::new(AppErrorCategory::Conflict, "多市场产物缺少策略身份列表")
            })?,
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场策略身份列表无效: {error}"),
        )
    })?;
    let spec_fingerprint = crate::multi_market_spec_fingerprint_v1(&spec, &strategy_identities)?;
    if spec_fingerprint != outcome.spec_fingerprint {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场 spec fingerprint 与 outcome 不一致",
        ));
    }
    let manifest_path = artifact
        .get("run_manifest_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AppError::new(
                AppErrorCategory::Conflict,
                "多市场产物缺少 RunManifest 路径",
            )
        })?;
    let manifest_text = read_text_bounded(
        reader,
        manifest_path,
        MAX_ARTIFACT_BYTES,
        "多市场 RunManifest",
    )?;
    let manifest = qx_core::RunManifest::from_json(&manifest_text).map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场 RunManifest 无效: {error:?}"),
        )
    })?;
    crate::artifact_store::verify_canonical_run_spec_for_manifest(&manifest, &artifact).map_err(
        |error| {
            AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场 CanonicalRunSpec 复核失败: {error}"),
            )
        },
    )?;
    if manifest.run_id != outcome.run_id
        || manifest.data_fingerprint != outcome.data_fingerprint
        || manifest.result_hash != outcome.shared_result_hash
        || manifest.input_event_hash != outcome.data_fingerprint
        || manifest.strategy_version != format!("multi-market-strategy-set:{spec_fingerprint}")
        || manifest.global_seed != spec.seed
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场 RunManifest 与 outcome/spec 身份不一致",
        ));
    }
    let expected_ledger_entries = artifact
        .get("ledger_entries")
        .filter(|entries| entries.is_array())
        .ok_or_else(|| {
            AppError::new(
                AppErrorCategory::Conflict,
                "多市场产物 ledger_entries 必须是数组",
            )
        })?;
    if expected_ledger_entries
        .as_array()
        .is_some_and(|entries| entries.len() > MAX_ARTIFACT_ROWS as usize)
    {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            "多市场产物 ledger_entries 超过行数上限",
        ));
    }
    let mut strategy_markets = BTreeMap::new();
    for market in &spec.markets {
        for strategy in &market.strategies {
            if strategy_markets
                .insert(
                    strategy.strategy.strategy_id.clone(),
                    market.instrument.clone(),
                )
                .is_some()
            {
                return Err(AppError::new(
                    AppErrorCategory::Conflict,
                    "多市场策略标识重复，不能唯一归属事件标的",
                ));
            }
        }
    }
    let page = request.page.map(|page| (page.offset, page.limit));
    let report = if artifact_version == Some(2) {
        if artifact.get("event_log").is_some() {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                "多市场 v2 产物不得在 JSON 中嵌入完整 event_log",
            ));
        }
        let event_trace_path = artifact
            .get("event_trace_path")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::Conflict,
                    "多市场 v2 产物缺少 event_trace_path",
                )
            })?;
        let expected_count = artifact
            .get("event_count")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                AppError::new(AppErrorCategory::Conflict, "多市场 v2 产物缺少 event_count")
            })?;
        if page.is_some_and(|(offset, _)| offset > expected_count) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "事件报告分页 offset 超出多市场事件流范围",
            ));
        }
        read_multi_market_event_report_v1_with_reader(
            event_trace_path,
            expected_count,
            &manifest.output_event_hash,
            expected_ledger_entries,
            &strategy_markets,
            page,
            reader,
        )?
    } else {
        let events = artifact
            .get("event_log")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                AppError::new(AppErrorCategory::Conflict, "多市场 v1 产物缺少 event_log")
            })?;
        read_multi_market_legacy_event_report_v1(
            events,
            &manifest.output_event_hash,
            expected_ledger_entries,
            &strategy_markets,
            page,
        )?
    };
    if report.fill_count != outcome.successful_fills {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场 Filled 事件数与 outcome 成交数不一致",
        ));
    }
    let replayed_ledger_entries = u64::try_from(report.ledger.entries().len()).map_err(|_| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            "多市场重放账簿条目数超出 u64 范围",
        )
    })?;
    let page = MultiMarketEventReportPageV1 {
        schema_version: MULTI_MARKET_EVENT_REPORT_SCHEMA_VERSION_V1,
        run_id: outcome.run_id.clone(),
        total: report.total,
        offset: report.offset,
        truncated: report.truncated,
        rows: report.rows,
        fill_count: report.fill_count,
        fill_sequence_sha256: report.fill_sequence_sha256,
        order_markets: report.order_markets,
        replayed_ledger_entries,
    };
    Ok(MultiMarketEventReportReadModelV1 {
        artifact,
        spec,
        manifest,
        page,
        ledger: report.ledger,
    })
}

/// Local-file convenience wrapper around [`read_multi_market_event_report_page_v1_with_reader`].
pub fn read_multi_market_event_report_page_v1(
    request: &MultiMarketEventReportReadRequestV1,
    context: &RunContext,
) -> Result<MultiMarketEventReportPageV1, AppError> {
    read_multi_market_event_report_page_v1_with_reader(request, context, &FileArtifactReader)
}

/// Read a replay-verified multi-market Depth page using local artifact paths.
pub fn read_multi_market_depth_event_report_page_v1(
    request: &MultiMarketDepthEventReportReadRequestV1,
    context: &RunContext,
) -> Result<MultiMarketEventReportPageV1, AppError> {
    read_multi_market_depth_event_report_page_v1_with_reader(request, context, &FileArtifactReader)
}

/// Read and verify the identity-bearing multi-market Depth report inputs through an authorized
/// artifact port. The complete event stream is replayed before the bounded page is returned.
pub fn read_multi_market_depth_event_report_page_v1_with_reader(
    request: &MultiMarketDepthEventReportReadRequestV1,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<MultiMarketEventReportPageV1, AppError> {
    read_multi_market_depth_event_report_read_model_v1_with_reader(request, context, reader)
        .map(|model| model.page)
}

/// Verify a multi-market Depth artifact once and retain its bounded page plus the verified
/// artifact/spec/manifest used by an API adapter to add authorized market-data projections.
pub fn read_multi_market_depth_event_report_read_model_v1_with_reader(
    request: &MultiMarketDepthEventReportReadRequestV1,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<MultiMarketDepthEventReportReadModelV1, AppError> {
    context.require(CallerCapability::Research, "多市场深度事件报告读取")?;
    if request.schema_version != MULTI_MARKET_EVENT_REPORT_SCHEMA_VERSION_V1 {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "多市场深度事件报告读取请求 schema_version 不受支持",
        ));
    }
    let outcome = &request.outcome;
    if outcome.run_id.trim().is_empty() || outcome.artifact_path.trim().is_empty() {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "多市场深度事件报告读取请求缺少有效 outcome 身份",
        ));
    }
    let artifact_text = read_text_bounded(
        reader,
        &outcome.artifact_path,
        MAX_ARTIFACT_BYTES,
        "多市场深度回测产物",
    )?;
    let artifact: Value = serde_json::from_str(&artifact_text).map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场深度回测产物 JSON 无效: {error}"),
        )
    })?;
    let artifact_version = artifact.get("schema_version").and_then(Value::as_u64);
    if !matches!(artifact_version, Some(1..=4)) {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场深度产物 schema_version 不受支持",
        ));
    }
    let embedded_outcome: MultiMarketDepthBacktestOutcome =
        serde_json::from_value(artifact.get("outcome").cloned().ok_or_else(|| {
            AppError::new(AppErrorCategory::Conflict, "多市场深度产物缺少 outcome")
        })?)
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场深度产物 outcome 无效: {error}"),
            )
        })?;
    if embedded_outcome != *outcome {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "请求 outcome 与多市场深度产物身份不一致",
        ));
    }
    let spec: MultiMarketDepthBacktestSpec = serde_json::from_value(
        artifact
            .get("spec")
            .cloned()
            .ok_or_else(|| AppError::new(AppErrorCategory::Conflict, "多市场深度产物缺少 spec"))?,
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场深度 spec 无效: {error}"),
        )
    })?;
    spec.validate()?;
    if spec.run_id != outcome.run_id
        || spec.account_id != outcome.account_id
        || spec.markets.len() != outcome.markets_total
        || spec.markets.len() > crate::MAX_MULTI_MARKET_DEPTH_MARKETS
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场深度 spec 与 outcome 摘要不一致",
        ));
    }
    let strategy_identities: Vec<StrategyArtifactIdentity> = serde_json::from_value(
        artifact
            .get("strategy_identities")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    )
    .map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场深度策略身份列表无效: {error}"),
        )
    })?;
    let spec_fingerprint =
        crate::multi_market_depth_spec_fingerprint_v1(&spec, &strategy_identities)?;
    if spec_fingerprint != outcome.spec_fingerprint {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场深度 spec fingerprint 与 outcome 不一致",
        ));
    }
    let mut identity_spec = spec.clone();
    identity_spec.output_dir.clear();
    for market in &mut identity_spec.markets {
        market.depth_path.clear();
        market.dataset_manifest_path = None;
    }
    let config_bytes = serde_json::to_vec(&identity_spec).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多市场深度配置编码失败: {error}"),
        )
    })?;
    let config_hash = qx_strategy::sha256_hex(&config_bytes);
    let manifest_path = artifact
        .get("run_manifest_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            AppError::new(
                AppErrorCategory::Conflict,
                "多市场深度产物缺少 RunManifest 路径",
            )
        })?;
    let manifest_text = read_text_bounded(
        reader,
        manifest_path,
        MAX_ARTIFACT_BYTES,
        "多市场深度 RunManifest",
    )?;
    let manifest = qx_core::RunManifest::from_json(&manifest_text).map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!("多市场深度 RunManifest 无效: {error:?}"),
        )
    })?;
    crate::artifact_store::verify_canonical_run_spec_for_manifest(&manifest, &artifact).map_err(
        |error| {
            AppError::new(
                AppErrorCategory::Conflict,
                format!("多市场深度 CanonicalRunSpec 复核失败: {error}"),
            )
        },
    )?;
    if manifest.run_id != outcome.run_id
        || manifest.config_hash != config_hash
        || manifest.data_fingerprint != outcome.data_fingerprint
        || manifest.result_hash != outcome.shared_result_hash
        || manifest.input_event_hash != outcome.data_fingerprint
        || manifest.strategy_version != format!("multi-market-depth:{spec_fingerprint}")
        || manifest.instrument_spec_version != "multi-market-depth-spec-v1"
        || manifest.model_fingerprint
            != qx_strategy::sha256_hex(
                b"OrderBookPortfolioBacktestEngine/OrderBookExecutionModel/v1",
            )
        || manifest.global_seed != 0
        || !manifest.determinism_mode
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场深度 RunManifest 与 outcome/spec 身份不一致",
        ));
    }
    if !outcome.execution_models.is_empty() {
        let declared = spec
            .markets
            .iter()
            .map(|market| {
                Ok((
                    market.instrument.clone(),
                    market.execution_model()?.audit_descriptor(),
                ))
            })
            .collect::<Result<BTreeMap<_, _>, AppError>>()?;
        if outcome.execution_models != declared {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                "多市场深度 outcome 生效撮合参数与 spec 不一致",
            ));
        }
    }
    let expected_ledger_entries = artifact
        .get("ledger_entries")
        .filter(|entries| entries.is_array())
        .ok_or_else(|| {
            AppError::new(
                AppErrorCategory::Conflict,
                "多市场深度产物 ledger_entries 必须是数组",
            )
        })?;
    if expected_ledger_entries
        .as_array()
        .is_some_and(|entries| entries.len() > MAX_ARTIFACT_ROWS as usize)
    {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            "多市场深度产物 ledger_entries 超过行数上限",
        ));
    }
    let mut strategy_markets = BTreeMap::new();
    for market in &spec.markets {
        if strategy_markets
            .insert(
                market.strategy.strategy_id.clone(),
                market.instrument.clone(),
            )
            .is_some()
        {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                "多市场深度策略标识重复，不能唯一归属事件标的",
            ));
        }
    }
    let requested_page = request.page.map(|page| (page.offset, page.limit));
    let report = if matches!(artifact_version, Some(2..=4)) {
        if artifact.get("event_log").is_some() {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                "多市场深度 v2 产物不得在 JSON 中嵌入完整 event_log",
            ));
        }
        let event_trace_path = artifact
            .get("event_trace_path")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::Conflict,
                    "多市场深度 v2 产物缺少 event_trace_path",
                )
            })?;
        let expected_count = artifact
            .get("event_count")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::Conflict,
                    "多市场深度 v2 产物缺少 event_count",
                )
            })?;
        if requested_page.is_some_and(|(offset, _)| offset > expected_count) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "事件报告分页 offset 超出多市场深度事件流范围",
            ));
        }
        read_multi_market_event_report_v1_with_reader(
            event_trace_path,
            expected_count,
            &manifest.output_event_hash,
            expected_ledger_entries,
            &strategy_markets,
            requested_page,
            reader,
        )?
    } else {
        let events = artifact
            .get("event_log")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                AppError::new(
                    AppErrorCategory::Conflict,
                    "多市场深度 v1 产物缺少 event_log",
                )
            })?;
        read_multi_market_legacy_event_report_v1(
            events,
            &manifest.output_event_hash,
            expected_ledger_entries,
            &strategy_markets,
            requested_page,
        )?
    };
    if report.fill_count != outcome.successful_fills {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场深度 Filled 事件数与 outcome 成交数不一致",
        ));
    }
    let replayed_ledger_entries = u64::try_from(report.ledger.entries().len()).map_err(|_| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            "多市场深度重放账簿条目数超出 u64 范围",
        )
    })?;
    let page = MultiMarketEventReportPageV1 {
        schema_version: MULTI_MARKET_EVENT_REPORT_SCHEMA_VERSION_V1,
        run_id: outcome.run_id.clone(),
        total: report.total,
        offset: report.offset,
        truncated: report.truncated,
        rows: report.rows,
        fill_count: report.fill_count,
        fill_sequence_sha256: report.fill_sequence_sha256,
        order_markets: report.order_markets,
        replayed_ledger_entries,
    };
    Ok(MultiMarketDepthEventReportReadModelV1 {
        artifact,
        spec,
        manifest,
        page,
    })
}

/// Verify and page a legacy multi-market artifact whose EventLog is embedded in its JSON value.
///
/// Newer artifacts store an event trace separately and use
/// [`read_multi_market_event_report_v1_with_reader`]. This compatibility reader keeps the v1
/// event replay, fill identity and order-to-market facts in the same application layer.
pub fn read_multi_market_legacy_event_report_v1(
    events: &[Value],
    expected_digest: &str,
    expected_ledger_entries: &Value,
    strategy_markets: &BTreeMap<String, String>,
    requested_page: Option<(u64, usize)>,
) -> Result<VerifiedMultiMarketEventReportV1, AppError> {
    if events.len() as u64 > MAX_ARTIFACT_ROWS {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            "多市场内嵌事件流超过 500 万行上限",
        ));
    }
    let total = u64::try_from(events.len())
        .map_err(|_| malformed_event_log("多市场内嵌事件数超出 u64 范围"))?;
    if let Some((_, limit)) = requested_page {
        if limit == 0 || limit > MAX_EVENT_REPORT_PAGE_ROWS {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!("事件报告分页 limit 必须在 1 到 {MAX_EVENT_REPORT_PAGE_ROWS} 之间"),
            ));
        }
    }
    let page = requested_page.unwrap_or((
        total.saturating_sub(MAX_EVENT_REPORT_PAGE_ROWS as u64),
        MAX_EVENT_REPORT_PAGE_ROWS,
    ));
    if page.0 > total {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "事件报告分页 offset 超出多市场事件流范围",
        ));
    }
    let offset = page.0;
    let end = offset.saturating_add(page.1 as u64).min(total);
    let start_index = usize::try_from(offset)
        .map_err(|_| malformed_event_log("多市场事件分页 offset 超出本机索引范围"))?;
    let end_index = usize::try_from(end)
        .map_err(|_| malformed_event_log("多市场事件分页终点超出本机索引范围"))?;
    let rows = events[start_index..end_index].to_vec();
    let replay = crate::verify_event_replay_v1(events, expected_digest, expected_ledger_entries)?;
    let mut fill_count = 0_u64;
    let mut fill_digest = qx_strategy::file_digest::Sha256Accumulator::new();
    fill_digest.update(b"qx-event-fill-sequence-v1\0");
    let mut order_markets = BTreeMap::<u64, String>::new();
    for event in events {
        if let Some(fill_value) = event.pointer("/kind/Filled/fill") {
            let fill: qx_core::Fill =
                serde_json::from_value(fill_value.clone()).map_err(|error| {
                    let message = format!("多市场成交事件结构无效: {error}");
                    malformed_event_log(&message)
                })?;
            update_fill_sequence_digest(&mut fill_digest, &event_fill_identity(&fill))?;
            fill_count = fill_count
                .checked_add(1)
                .ok_or_else(|| malformed_event_log("多市场成交事件计数溢出"))?;
        }
        let (Some(order_id), Some(instrument)) = (
            event_order_id(event),
            event_market_instrument(event, strategy_markets, &BTreeMap::new()),
        ) else {
            continue;
        };
        if order_markets
            .get(&order_id)
            .is_some_and(|existing| existing != &instrument)
        {
            return Err(malformed_event_log(
                "同一订单在多市场内嵌事件流中关联到不同标的",
            ));
        }
        order_markets.insert(order_id, instrument);
    }
    Ok(VerifiedMultiMarketEventReportV1 {
        total,
        offset,
        truncated: (rows.len() as u64) < total,
        rows,
        fill_count,
        fill_sequence_sha256: fill_digest.finish_hex(),
        order_markets,
        ledger: replay.ledger,
    })
}

pub(super) fn read_multi_market_order_market_index_v1(
    reference: &str,
    expected_trace_sha256: &str,
    required_order_ids: &BTreeSet<u64>,
    strategy_markets: &BTreeMap<String, String>,
    reader: &dyn ArtifactReader,
) -> Result<BTreeMap<u64, String>, AppError> {
    let mut artifact = reader.open(reference)?;
    if artifact.byte_len > MAX_ARTIFACT_BYTES {
        return Err(artifact_too_large(reference, MAX_ARTIFACT_BYTES));
    }
    let mut bytes_seen = 0_u64;
    let mut total = 0_u64;
    let mut digest = qx_strategy::file_digest::Sha256Accumulator::new();
    digest.update(b"qx-multi-market-event-trace-bytes-v1\0");
    let mut order_markets = BTreeMap::<u64, String>::new();
    while let Some((line, line_bytes)) = read_bounded_line(artifact.reader.as_mut(), reference)? {
        bytes_seen = bytes_seen
            .checked_add(line_bytes as u64)
            .ok_or_else(|| malformed_event_log("多市场事件轨迹字节数溢出"))?;
        if bytes_seen > MAX_ARTIFACT_BYTES {
            return Err(artifact_too_large(reference, MAX_ARTIFACT_BYTES));
        }
        digest.update(line.as_bytes());
        if line.trim().is_empty() {
            continue;
        }
        total = total
            .checked_add(1)
            .ok_or_else(|| malformed_event_log("多市场事件轨迹行数溢出"))?;
        if total > MAX_ARTIFACT_ROWS {
            return Err(AppError::new(
                AppErrorCategory::Timeout,
                "多市场事件轨迹超过 500 万行上限",
            ));
        }
        let event: qx_core::Event = serde_json::from_str(line.trim_end()).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("多市场事件轨迹第 {total} 行格式损坏: {error}"),
            )
        })?;
        let Some(order_id) =
            event_order_id_typed(&event).filter(|order_id| required_order_ids.contains(order_id))
        else {
            continue;
        };
        let event_value = serde_json::to_value(&event).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("多市场事件行投影失败: {error}"),
            )
        })?;
        let Some(instrument) =
            event_market_instrument(&event_value, strategy_markets, &BTreeMap::new())
        else {
            continue;
        };
        if order_markets
            .get(&order_id)
            .is_some_and(|existing| existing != &instrument)
        {
            return Err(malformed_event_log(
                "同一订单在多市场事件轨迹中关联到不同标的",
            ));
        }
        order_markets.insert(order_id, instrument);
    }
    if digest.finish_hex() != expected_trace_sha256 {
        return Err(malformed_event_log(
            "多市场事件轨迹在分页归属复核期间发生变化",
        ));
    }
    Ok(order_markets)
}

fn event_order_id_typed(event: &qx_core::Event) -> Option<u64> {
    match &event.kind {
        qx_core::EventKind::OrderRiskDecision { decision } => Some(decision.client_order_id),
        qx_core::EventKind::OrderSubmitted { order } => Some(order.client_id),
        qx_core::EventKind::Submit { client_order_id }
        | qx_core::EventKind::Accepted {
            client_order_id, ..
        }
        | qx_core::EventKind::Rejected {
            client_order_id, ..
        }
        | qx_core::EventKind::CancelPending { client_order_id }
        | qx_core::EventKind::Cancelled { client_order_id }
        | qx_core::EventKind::ReconcileRequired { client_order_id } => Some(*client_order_id),
        qx_core::EventKind::Filled { fill } => Some(fill.order_id),
        qx_core::EventKind::LedgerApplied { entry } => entry.order_id,
        _ => None,
    }
}

fn event_market_instrument(
    event: &Value,
    strategy_markets: &BTreeMap<String, String>,
    order_markets: &BTreeMap<u64, String>,
) -> Option<String> {
    let kind = event.get("kind")?.as_object()?;
    if let Some(intents) = kind
        .get("StrategyDecision")
        .and_then(|event| event.get("decision"))
        .and_then(|decision| decision.get("intents"))
        .and_then(Value::as_array)
    {
        let intent_markets = intents
            .iter()
            .filter_map(|intent| intent.get("instrument").and_then(canonical_instrument))
            .collect::<std::collections::BTreeSet<_>>();
        if intent_markets.len() > 1 {
            return None;
        }
        if let Some(instrument) = intent_markets.into_iter().next() {
            return Some(instrument);
        }
    }
    let instrument = kind
        .get("OrderRiskDecision")
        .and_then(|event| event.get("decision"))
        .and_then(|decision| decision.get("instrument"))
        .or_else(|| {
            kind.get("OrderSubmitted")
                .and_then(|event| event.get("order"))
                .and_then(|order| order.get("instrument"))
        })
        .or_else(|| {
            kind.get("Filled")
                .and_then(|event| event.get("fill"))
                .and_then(|fill| fill.get("instrument"))
        })
        .or_else(|| {
            kind.get("LedgerApplied")
                .and_then(|event| event.get("entry"))
                .and_then(|entry| entry.get("instrument"))
        })
        .or_else(|| kind.values().find_map(|payload| payload.get("instrument")));
    if let Some(instrument) = instrument.and_then(canonical_instrument) {
        return Some(instrument);
    }
    if let Some(instrument) =
        event_order_id(event).and_then(|order_id| order_markets.get(&order_id))
    {
        return Some(instrument.clone());
    }
    let strategy_id = kind
        .get("OrderRiskDecision")
        .and_then(|event| event.get("decision"))
        .and_then(|decision| decision.get("strategy_id"))
        .and_then(Value::as_str)
        .or_else(|| {
            kind.get("StrategyDecision")
                .and_then(|event| event.get("decision"))
                .and_then(|decision| decision.get("strategy_id"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            event
                .pointer("/metadata/context/strategy_id")
                .and_then(Value::as_str)
        })?;
    strategy_markets.get(strategy_id).cloned()
}

fn canonical_instrument(instrument: &Value) -> Option<String> {
    serde_json::from_value::<qx_core::InstrumentId>(instrument.clone())
        .ok()
        .map(|instrument| instrument.to_string())
        .or_else(|| instrument.as_str().map(str::to_owned))
}

fn event_order_id(event: &Value) -> Option<u64> {
    let kind = event.get("kind")?.as_object()?;
    kind.get("OrderRiskDecision")
        .and_then(|event| event.get("decision"))
        .and_then(|decision| decision.get("client_order_id"))
        .or_else(|| {
            kind.get("OrderSubmitted")
                .and_then(|event| event.get("order"))
                .and_then(|order| order.get("client_id"))
        })
        .or_else(|| {
            kind.get("Filled")
                .and_then(|event| event.get("fill"))
                .and_then(|fill| fill.get("order_id"))
        })
        .or_else(|| {
            kind.get("LedgerApplied")
                .and_then(|event| event.get("entry"))
                .and_then(|entry| entry.get("order_id"))
        })
        .or_else(|| {
            [
                "Submit",
                "Accepted",
                "Rejected",
                "Cancelled",
                "ReconcileRequired",
            ]
            .iter()
            .find_map(|variant| {
                kind.get(*variant)
                    .and_then(|event| event.get("client_order_id"))
            })
        })
        .and_then(Value::as_u64)
}
