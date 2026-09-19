//! 生产就绪判定：研究快照绑定、API 就绪度、交易资产与 worker 凭据的 fail-closed 检查。
//!
//! 由 `main.rs` 的 crate 根职责簇拆出（Phase 4p），条目经根部的
//! `pub(crate) use readiness::*;` 再导出，行为与拆分前逐字相同。

use super::*;

pub(crate) fn validate_research_snapshot_binding(
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
        if validate_research_snapshot_binding(strategy, &research).is_err()
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

    let metrics_unhealthy = std::fs::read_dir(metrics_dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter_map(|entry| {
            (entry.path().extension().and_then(|value| value.to_str()) == Some("prom"))
                .then(|| std::fs::read_to_string(entry.path()).ok())
        })
        .flatten()
        .any(|content| worker_metrics_unhealthy(&content, now_ms, stale_after_ms));
    if metrics_unhealthy {
        return ApiReadiness {
            ready: false,
            detail: "worker_dependency_unavailable".into(),
        };
    }

    ApiReadiness {
        ready: true,
        detail: "dependencies_ready".into(),
    }
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
