//! Periodic, authenticated publication of this process's local deployment projection.

use qx_adapter::{HttpRequest, HttpTransport, TlsHttpTransport};
use qx_runtime::{RuntimeConfig, StrategyDeploymentPublisherRuntimeConfig};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

const PUBLISH_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PUBLISH_BODY_BYTES: usize = 900 * 1024;
const PUBLISH_PATH: &str = "/strategies/deployment-nodes";

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct StrategyDeploymentNodeSnapshotV1<'a> {
    schema_version: u32,
    generated_at_ms: u64,
    deployments: &'a [qx_api::StrategyDeploymentStatusV1],
}

/// Start the configured mTLS publisher. A configured publisher must be constructible before
/// serving starts; failures are returned to the host so an operator cannot mistake a silent
/// non-publisher for a reporting node.
pub fn spawn_strategy_deployment_publisher(
    config: RuntimeConfig,
    runtime_timestamp_ms: fn() -> u64,
    stop: Arc<AtomicBool>,
) -> Result<Option<JoinHandle<()>>, String> {
    let Some(publisher_config) = config.api.strategy_deployment_publisher.clone() else {
        return Ok(None);
    };
    let transport = TlsHttpTransport::from_mtls_pem(
        PUBLISH_TIMEOUT,
        &publisher_config.server_ca,
        &publisher_config.client_certificate_chain,
        &publisher_config.client_private_key,
    )?;
    let handle = thread::Builder::new()
        .name("strategy-deployment-publisher".into())
        .spawn(move || {
            while !stop.load(Ordering::Acquire) {
                let now_ms = runtime_timestamp_ms();
                if let Err(error) = publish_strategy_deployment_snapshot(
                    &config,
                    &publisher_config,
                    &transport,
                    now_ms,
                ) {
                    eprintln!("[运行时 · 部署发布] 本轮快照发布失败: {error}");
                }
                let interval = Duration::from_millis(publisher_config.interval_ms);
                let deadline = std::time::Instant::now() + interval;
                while !stop.load(Ordering::Acquire) {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    thread::sleep(remaining.min(Duration::from_millis(100)));
                }
            }
        })
        .map_err(|error| format!("启动策略部署快照 publisher 失败: {error}"))?;
    Ok(Some(handle))
}

/// Publish one local observation using the configured server trust root and client identity.
pub fn publish_strategy_deployment_snapshot(
    config: &RuntimeConfig,
    publisher: &StrategyDeploymentPublisherRuntimeConfig,
    transport: &dyn HttpTransport,
    timestamp_ms: u64,
) -> Result<(), String> {
    if timestamp_ms == 0 {
        return Err("snapshot timestamp must be non-zero".into());
    }
    let deployments = crate::strategy_deployments::read_strategy_deployments(config, timestamp_ms)?;
    if deployments.len() > 1024 {
        return Err("local deployment projection exceeds 1024 workers".into());
    }
    let body = serde_json::to_string(&StrategyDeploymentNodeSnapshotV1 {
        schema_version: 1,
        generated_at_ms: timestamp_ms,
        deployments: &deployments,
    })
    .map_err(|error| format!("序列化策略部署快照失败: {error}"))?;
    if body.len() > MAX_PUBLISH_BODY_BYTES {
        return Err(format!(
            "snapshot body exceeds {} bytes",
            MAX_PUBLISH_BODY_BYTES
        ));
    }
    let response = transport.send(HttpRequest {
        method: "POST".into(),
        host: publisher.server_host.clone(),
        port: publisher.server_port,
        path: PUBLISH_PATH.into(),
        body,
        headers: BTreeMap::new(),
    })?;
    if response.status != 202 {
        return Err(format!(
            "aggregator rejected snapshot with HTTP {}: {}",
            response.status,
            response.body.chars().take(512).collect::<String>()
        ));
    }
    Ok(())
}
