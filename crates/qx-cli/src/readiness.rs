//! 生产就绪判定：研究快照绑定、API 就绪度、交易资产与 worker 凭据的 fail-closed 检查。
//!
//! 由 `main.rs` 的 crate 根职责簇拆出（Phase 4p），条目经根部的
//! `pub(crate) use readiness::*;` 再导出，行为与拆分前逐字相同。

use super::*;

pub(crate) fn validate_research_snapshot_binding(
    root: &Path,
    strategy: &StrategyRuntimeConfig,
    research: &StrategyResearchSnapshot,
) -> Result<(), String> {
    if let Some(expected) = strategy.research_data_fingerprint.as_deref() {
        let actual = research.candidate.config.data_fingerprint.as_str();
        if actual != expected {
            return Err(format!(
                "research snapshot data_fingerprint 不匹配: expected={expected} actual={actual}"
            ));
        }
    }
    verify_event_backtest_evidence(root, research)
}

/// 复核研究快照声明的事件回测证据（V12 §18-B #115）。
///
/// `event_verified` 一度只是快照 JSON 里手抄的一格摘要数字：运行时从不回到 `runs/` 找那本
/// RunManifest，也不按内容重算摘要，于是编造一个 `u64` 就能让实盘闸门以为自己握着事件回测
/// 证据。现在声明必须能在同一份 runtime 的 `runs/` 里指到一本清单，且**按文件内容**重算出的
/// 摘要与声明一致，清单的标的血缘与验证区间也必须来自同一次事件回测。
pub(crate) fn verify_event_backtest_evidence(
    root: &Path,
    research: &StrategyResearchSnapshot,
) -> Result<(), String> {
    let candidate = &research.candidate;
    if !candidate.event_verified {
        return Ok(());
    }
    let declared = candidate.event_manifest_digest.ok_or_else(|| {
        "研究快照声明 event_verified 却没写事件回测 RunManifest 摘要，闸门无法复核这种声明"
            .to_string()
    })?;
    let runs_root = root.join("runs");
    let suffix = format!("-{declared:016x}.run.json");
    let entries = std::fs::read_dir(&runs_root).map_err(|_| {
        format!(
            "研究快照声明了事件回测证据，但回测产物目录不存在或读不开 {}，摘要 {declared:016x} 无处可查",
            runs_root.display()
        )
    })?;
    let mut evidence: Option<RunManifest> = None;
    for entry in entries {
        let path = entry
            .map_err(|error| format!("遍历回测产物目录失败 {}: {error}", runs_root.display()))?
            .path();
        if !path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.ends_with(&suffix))
        {
            continue;
        }
        let manifest =
            RunManifest::from_json(&std::fs::read_to_string(&path).map_err(|error| {
                format!("读取事件回测 RunManifest 失败 {}: {error}", path.display())
            })?)
            .map_err(|error| format!("事件回测 RunManifest 无效 {}: {error}", path.display()))?;
        // 「什么算证据摘要」只在 qx-factor 的盖章入口定义一次：这里用它重算一遍，
        // 而不是在运行时抄一份 digest 算法，否则两侧口径分叉时文件名永远对得上自己。
        let stamped = candidate
            .clone()
            .mark_event_verified(&manifest)
            .map_err(|error| {
                format!(
                    "事件回测 RunManifest 不能作为证据 {}: {error:?}",
                    path.display()
                )
            })?;
        if stamped.event_manifest_digest != candidate.event_manifest_digest {
            return Err(format!(
                "事件回测 RunManifest 的文件名声称摘要 {declared:016x}，按内容重算得到 {:?}（{}）",
                stamped.event_manifest_digest,
                path.display()
            ));
        }
        evidence = Some(manifest);
    }
    let manifest = evidence.ok_or_else(|| {
        format!(
            "研究快照声明的事件回测证据指不到真实产物：{} 里没有摘要 {declared:016x} 的 RunManifest",
            runs_root.display()
        )
    })?;
    for (field, declared_value, evidence_value) in [
        (
            "strategy_version",
            candidate.config.strategy_version.as_str(),
            manifest.strategy_version.as_str(),
        ),
        (
            "data_fingerprint",
            candidate.config.data_fingerprint.as_str(),
            manifest.data_fingerprint.as_str(),
        ),
    ] {
        if declared_value != evidence_value {
            return Err(format!(
                "事件回测证据与 candidate 的 {field} 不符: candidate={declared_value} manifest={evidence_value}"
            ));
        }
    }
    if manifest.clock_start > candidate.validation_start
        || manifest.clock_end < candidate.validation_end
    {
        return Err(format!(
            "事件回测证据的时钟区间 [{}, {}] 没有覆盖 candidate 的验证窗口 [{}, {}]",
            manifest.clock_start,
            manifest.clock_end,
            candidate.validation_start,
            candidate.validation_end
        ));
    }
    Ok(())
}

pub(crate) fn configured_api_readiness(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
    control_store: &ControlStateBackend,
    metrics_dir: &Path,
    now_ms: u64,
    stale_after_ms: u64,
) -> ApiReadiness {
    if control_store.load().is_err() {
        return ApiReadiness {
            ready: false,
            detail: "control_store_unavailable".into(),
        };
    }

    if config.environment.eq_ignore_ascii_case("production")
        && !production_trading_assets_ready(config, runtime_config_path)
    {
        return ApiReadiness {
            ready: false,
            detail: "trading_safety_assets_unavailable".into(),
        };
    }

    let strategies = if config.strategies.is_empty() {
        std::slice::from_ref(&config.strategy)
    } else {
        config.strategies.as_slice()
    };
    let root = Path::new(&config.storage.data_dir);
    for strategy in strategies {
        if !strategy.research_snapshot_required {
            continue;
        }
        let Some(configured) = strategy.research_snapshot_path.as_deref() else {
            return ApiReadiness {
                ready: false,
                detail: "research_snapshot_unavailable".into(),
            };
        };
        let path = resolve_runtime_asset_path(runtime_config_path, root, configured);
        if !path.is_file() {
            return ApiReadiness {
                ready: false,
                detail: "research_snapshot_unavailable".into(),
            };
        }
        let payload = match std::fs::read_to_string(&path) {
            Ok(payload) => payload,
            Err(_) => {
                return ApiReadiness {
                    ready: false,
                    detail: "research_snapshot_unreadable".into(),
                }
            }
        };
        let research = match StrategyResearchSnapshot::from_json(&payload) {
            Ok(research) => research,
            Err(_) => {
                return ApiReadiness {
                    ready: false,
                    detail: "research_snapshot_invalid".into(),
                }
            }
        };
        if validate_research_snapshot_binding(root, strategy, &research).is_err()
            || research
                .validate_for(
                    &strategy.version,
                    strategy
                        .research_data_fingerprint
                        .as_deref()
                        .unwrap_or(research.candidate.config.data_fingerprint.as_str()),
                    now_ms,
                    config.environment.eq_ignore_ascii_case("production"),
                )
                .is_err()
        {
            return ApiReadiness {
                ready: false,
                detail: "research_snapshot_invalid".into(),
            };
        }
    }

    let published_metrics = std::fs::read_dir(metrics_dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("prom"))
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .collect::<Vec<_>>();
    let unhealthy = published_metrics
        .iter()
        .any(|content| worker_metrics_unhealthy(content, now_ms, stale_after_ms));
    let workers_expected = config.workers.iter().any(|worker| worker.enabled);
    if let Some(detail) =
        worker_metrics_readiness(published_metrics.len(), unhealthy, workers_expected)
    {
        return ApiReadiness {
            ready: false,
            detail: detail.into(),
        };
    }

    ApiReadiness {
        ready: true,
        detail: "dependencies_ready".into(),
    }
}

/// 指标侧的就绪结论（`Some` = 不就绪的原因）：一份 `.prom` 都没扫到不等于依赖健康。
///
/// 空目录既可能是"这个部署本来没有 worker"，也可能是 worker 从未起来、或 API 与 worker
/// 读的不是同一个 `data_dir`（V10 Q55 报告过的双落点）。后者必须报"没有证据"，否则
/// `/ready` 会替从未发布过健康的 worker 宣称就绪。
pub(crate) fn worker_metrics_readiness(
    published: usize,
    unhealthy: bool,
    workers_expected: bool,
) -> Option<&'static str> {
    if unhealthy {
        return Some("worker_dependency_unavailable");
    }
    (published == 0 && workers_expected).then_some("worker_metrics_unpublished")
}

pub(crate) fn non_empty_env(name: &str) -> bool {
    !name.trim().is_empty() && std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
}

pub(crate) fn production_trading_assets_ready(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
) -> bool {
    config
        .workers
        .iter()
        .filter(|worker| {
            worker.enabled
                && matches!(
                    worker.role,
                    WorkerRole::UserStream
                        | WorkerRole::Execution
                        | WorkerRole::SpreadRecovery
                        | WorkerRole::Reconciler
                )
        })
        .all(|worker| {
            let credentials_ready =
                worker_credentials_ready(runtime_config_path, worker).unwrap_or(false);
            if !credentials_ready {
                return false;
            }
            worker.instrument_spec_path.as_deref().is_none_or(|path| {
                resolve_runtime_relative_path(runtime_config_path, path).is_file()
            })
        })
}

/// 检查私有 worker 的密钥来源，兼容三种部署方式：RuntimeConfig 环境变量、
/// RuntimeConfig 凭据文件，以及公共 CCXT JSON 配置中的 `credential_env`。
/// 这里只验证引用和环境变量是否可用，永远不返回或打印密钥内容。
pub(crate) fn worker_credentials_ready(
    runtime_config_path: &Path,
    worker: &WorkerConfig,
) -> Result<bool, String> {
    // CCXT worker 实际由 endpoint 配置驱动；当 endpoint 是本地 JSON 时，
    // 优先读取其中的 credential_env，避免 RuntimeConfig 中的兼容字段
    // 覆盖真正会被公共 CCXT 进程使用的凭据来源。
    let ccxt_endpoint = worker
        .endpoint
        .as_deref()
        .filter(|endpoint| !endpoint.contains("://") && worker.role.is_venue_role());
    if let Some(endpoint) = ccxt_endpoint {
        let endpoint_path = resolve_runtime_relative_path(runtime_config_path, endpoint);
        let payload = std::fs::read_to_string(&endpoint_path).map_err(|error| {
            format!(
                "读取 worker {} CCXT 配置失败 {}: {error}",
                worker.id,
                endpoint_path.display()
            )
        })?;
        let value = serde_json::from_str::<serde_json::Value>(&payload).map_err(|error| {
            format!(
                "解析 worker {} CCXT 配置失败 {}: {error}",
                worker.id,
                endpoint_path.display()
            )
        })?;
        let Some(credentials) = value.get("credential_env") else {
            return Ok(false);
        };
        let env_value = |key: &str| {
            credentials
                .get(key)
                .and_then(serde_json::Value::as_str)
                .is_some_and(non_empty_env)
        };
        let optional_env_value = |key: &str| {
            credentials
                .get(key)
                .and_then(serde_json::Value::as_str)
                .is_none_or(non_empty_env)
        };
        return Ok(env_value("api_key") && env_value("secret") && optional_env_value("password"));
    }
    if let Some(credentials) = worker.credential_env.as_ref() {
        return Ok(non_empty_env(&credentials.api_key) && non_empty_env(&credentials.secret));
    }
    if let Some(credentials) = worker.credential_files.as_ref() {
        return Ok(
            resolve_runtime_relative_path(runtime_config_path, &credentials.api_key).is_file()
                && resolve_runtime_relative_path(runtime_config_path, &credentials.secret)
                    .is_file(),
        );
    }
    let Some(endpoint) = worker.endpoint.as_deref() else {
        return Ok(false);
    };
    if endpoint.contains("://") || endpoint.starts_with("ws:") || endpoint.starts_with("wss:") {
        return Ok(false);
    }
    let endpoint_path = resolve_runtime_relative_path(runtime_config_path, endpoint);
    let payload = std::fs::read_to_string(&endpoint_path).map_err(|error| {
        format!(
            "读取 worker {} CCXT 配置失败 {}: {error}",
            worker.id,
            endpoint_path.display()
        )
    })?;
    let value = serde_json::from_str::<serde_json::Value>(&payload).map_err(|error| {
        format!(
            "解析 worker {} CCXT 配置失败 {}: {error}",
            worker.id,
            endpoint_path.display()
        )
    })?;
    let Some(credentials) = value.get("credential_env") else {
        return Ok(false);
    };
    let env_value = |key: &str| {
        credentials
            .get(key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(non_empty_env)
    };
    let optional_env_value = |key: &str| {
        credentials
            .get(key)
            .and_then(serde_json::Value::as_str)
            .is_none_or(non_empty_env)
    };
    Ok(env_value("api_key") && env_value("secret") && optional_env_value("password"))
}

#[cfg(feature = "nats")]
pub(crate) fn prometheus_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}
