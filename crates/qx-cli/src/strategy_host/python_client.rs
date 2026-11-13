use super::imports::*;
use super::SandboxExecutionPolicy;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(crate) struct PythonStrategyClient {
    pub(super) worker: qx_orchestrator::StrategyWorkerProcess,
    pub(super) transport: StrategyTransport,
    pub(super) expects_strategy_checkpoint: bool,
    pub(super) last_strategy_checkpoint: Option<qx_strategy::contract::StrategyCheckpointV1>,
}

pub(crate) type StrategyProcessClient = PythonStrategyClient;

impl PythonStrategyClient {
    fn start(module: &str, timeout_ms: u64) -> Result<Self, String> {
        Self::start_with_transport(module, timeout_ms, StrategyTransport::Jsonl)
    }

    fn start_with_transport(
        module: &str,
        timeout_ms: u64,
        transport: StrategyTransport,
    ) -> Result<Self, String> {
        Self::start_with_transport_config(
            module,
            timeout_ms,
            transport,
            SharedRingConfig::default(),
            None,
        )
    }

    pub(crate) fn start_with_transport_config(
        module: &str,
        timeout_ms: u64,
        transport: StrategyTransport,
        ring_config: SharedRingConfig,
        artifact_sha256: Option<&str>,
    ) -> Result<Self, String> {
        Self::start_with_transport_config_and_ring_directory(
            module,
            timeout_ms,
            transport,
            ring_config,
            artifact_sha256,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
        )
    }

    pub(crate) fn start_with_strategy_config_and_audit_path(
        module: &str,
        timeout_ms: u64,
        transport: StrategyTransport,
        ring_config: SharedRingConfig,
        artifact_sha256: Option<&str>,
        strategy_spec: Option<&serde_json::Value>,
        account_id: Option<&str>,
        venue_id: Option<&str>,
        sandbox_audit_path: Option<&Path>,
        host_worker_id: Option<&str>,
        host_capabilities: Option<&qx_strategy::capabilities::StrategyHostCapabilitiesV1>,
    ) -> Result<Self, String> {
        Self::start_with_transport_config_and_ring_directory(
            module,
            timeout_ms,
            transport,
            ring_config,
            artifact_sha256,
            None,
            strategy_spec,
            account_id,
            venue_id,
            sandbox_audit_path,
            host_worker_id,
            false,
            host_capabilities,
        )
    }

    #[cfg(test)]
    pub(crate) fn start_with_transport_config_in(
        module: &str,
        timeout_ms: u64,
        transport: StrategyTransport,
        ring_config: SharedRingConfig,
        artifact_sha256: Option<&str>,
        ring_directory: &Path,
    ) -> Result<Self, String> {
        Self::start_with_transport_config_and_ring_directory(
            module,
            timeout_ms,
            transport,
            ring_config,
            artifact_sha256,
            Some(ring_directory),
            None,
            None,
            None,
            None,
            None,
            false,
            None,
        )
    }

    pub(crate) fn start_for_runtime_worker(
        module: &str,
        timeout_ms: u64,
        transport: StrategyTransport,
        ring_config: SharedRingConfig,
        artifact_sha256: Option<&str>,
        strategy_spec: Option<&serde_json::Value>,
        account_id: Option<&str>,
        venue_id: Option<&str>,
        sandbox_audit_path: Option<&Path>,
        host_worker_id: Option<&str>,
    ) -> Result<Self, String> {
        Self::start_with_transport_config_and_ring_directory(
            module,
            timeout_ms,
            transport,
            ring_config,
            artifact_sha256,
            None,
            strategy_spec,
            account_id,
            venue_id,
            sandbox_audit_path,
            host_worker_id,
            true,
            None,
        )
    }

    fn start_with_transport_config_and_ring_directory(
        module: &str,
        timeout_ms: u64,
        transport: StrategyTransport,
        ring_config: SharedRingConfig,
        artifact_sha256: Option<&str>,
        ring_directory: Option<&Path>,
        strategy_spec: Option<&serde_json::Value>,
        account_id: Option<&str>,
        venue_id: Option<&str>,
        sandbox_audit_path: Option<&Path>,
        host_worker_id: Option<&str>,
        runtime_delivery: bool,
        explicit_host_capabilities: Option<&qx_strategy::capabilities::StrategyHostCapabilitiesV1>,
    ) -> Result<Self, String> {
        // Sandbox policy is a deployment-owned host setting. Strategy specs cannot
        // opt themselves out of a server's remote-execution boundary. Local stays
        // the default for desktop research and trusted local development.
        let execution_policy = SandboxExecutionPolicy::from_env()?;
        let execution_mode = execution_policy.provider();
        let remote_execution = execution_policy.sandbox_enabled();
        if remote_execution
            && matches!(
                transport,
                StrategyTransport::SharedMemoryJson | StrategyTransport::SharedMemoryColumnar
            )
        {
            return Err(
                "remote Python strategy modes currently support jsonl and framed_json transports; shared-memory transport is local-only".into(),
            );
        }
        if remote_execution && timeout_ms < 5_000 {
            return Err(
                "remote Python strategy modes require strategy.python_timeout_ms >= 5000 for provider request latency".into(),
            );
        }
        if execution_mode == "opensandbox" && timeout_ms < 15_000 {
            return Err(
                "OpenSandbox strategy mode requires strategy.python_timeout_ms >= 15000 for file and command API round trips".into(),
            );
        }
        if execution_mode == "llm-sandbox" && timeout_ms < 15_000 {
            return Err(
                "llm-sandbox strategy mode requires strategy.python_timeout_ms >= 15000 for remote Docker and interactive-kernel round trips".into(),
            );
        }

        let (python, interpreter_origin) = python_interpreter_origin();
        let python_path = python_module_search_path()?;
        let mut env = BTreeMap::new();
        let mut host_provider_env = BTreeMap::new();
        if remote_execution {
            if let Some(path) = sandbox_audit_path {
                env.insert(
                    "QX_STRATEGY_SANDBOX_AUDIT_PATH".into(),
                    path.to_string_lossy().into_owned(),
                );
                env.insert("QX_STRATEGY_SANDBOX_AUDIT_REQUIRED".into(), "true".into());
                if let Some(worker_id) = host_worker_id {
                    env.insert("QX_STRATEGY_HOST_WORKER_ID".into(), worker_id.to_string());
                }
            }
        }
        env.insert(
            "PYTHONPATH".to_string(),
            python_path.to_string_lossy().into_owned(),
        );
        if let Some(root) = frozen_python_dependency_root(module) {
            env.insert(
                "QX_STRATEGY_DEPENDENCY_ROOT".into(),
                root.to_string_lossy().into_owned(),
            );
        }
        if let Some(artifact_sha256) = artifact_sha256 {
            env.insert(
                "QX_STRATEGY_ARTIFACT_SHA256".to_string(),
                artifact_sha256.to_string(),
            );
        }
        if runtime_delivery {
            env.insert("QX_STRATEGY_REQUIRE_CHECKPOINT".into(), "true".into());
        }
        let runtime_capabilities = if runtime_delivery {
            Some(match strategy_spec {
                Some(strategy_spec) => {
                    let data_level = strategy_spec
                        .get("data_level")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("bar");
                    qx_strategy::capabilities::StrategyHostCapabilitiesV1::runtime_python_class(
                        data_level,
                        account_id.is_some() && venue_id.is_some(),
                    )?
                }
                None => qx_strategy::capabilities::StrategyHostCapabilitiesV1::runtime_python_module_function(
                    account_id.is_some() && venue_id.is_some(),
                ),
            })
        } else {
            None
        };
        let host_capabilities = runtime_capabilities.as_ref().or(explicit_host_capabilities);
        if let Some(capabilities) = host_capabilities {
            capabilities.validate_required(&[])?;
            env.insert(
                "QX_STRATEGY_HOST_CAPABILITIES_JSON".into(),
                serde_json::to_string(&capabilities).map_err(|error| error.to_string())?,
            );
        }
        if let Some(strategy_spec) = strategy_spec {
            let encoded = serde_json::to_string(strategy_spec)
                .map_err(|error| format!("Python strategy spec 序列化失败: {error}"))?;
            if encoded.len() > 16 * 1024 {
                return Err("Python strategy spec 超过 16 KiB worker 配置上限".into());
            }
            env.insert("QX_STRATEGY_SPEC_JSON".into(), encoded);
        }
        if let Some(account_id) = account_id {
            env.insert("QX_STRATEGY_ACCOUNT_ID".into(), account_id.to_string());
        }
        if let Some(venue_id) = venue_id {
            env.insert("QX_STRATEGY_VENUE_ID".into(), venue_id.to_string());
        }
        let worker_module = super::providers::configure(
            execution_mode,
            timeout_ms,
            &mut env,
            &mut host_provider_env,
        )?;
        let mut args = vec![
            "-X".into(),
            "utf8".into(),
            "-m".into(),
            worker_module.into(),
            "--module".into(),
            module.into(),
        ];
        if transport == StrategyTransport::FramedJson {
            args.push("--protocol".into());
            args.push("framed_json".into());
        }
        let ready_path = if let (false, Some(host_worker_id), Some(_)) =
            (remote_execution, host_worker_id, strategy_spec)
        {
            let path = sandbox_audit_path
                .map_or_else(
                    || super::sandbox_audit_path(&std::env::temp_dir(), host_worker_id),
                    Path::to_path_buf,
                )
                .with_extension("ready");
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            // An exclusive reservation proves this invocation owns a fresh marker.
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|error| format!("cannot reserve strategy startup marker: {error}"))?;
            args.extend(["--ready-file".into(), path.to_string_lossy().into_owned()]);
            Some(path)
        } else {
            None
        };
        // Remote startup includes sandbox creation, source/data transfer and a
        // package preflight. Give that one-time bridge startup a bounded budget
        // beyond the strategy's per-request timeout; each provider bridge still
        // enforces the configured timeout for every strategy response.
        let worker_timeout_ms = if remote_execution {
            timeout_ms.saturating_add(240_000)
        } else {
            timeout_ms
        };
        let spawned = Self::start_process_with_ring_directory_and_host_provider_env(
            &python,
            &args,
            &env,
            worker_timeout_ms,
            "Python Strategy",
            transport,
            ring_config,
            Some(interpreter_origin),
            ring_directory,
            &host_provider_env,
            None,
            None,
        );
        let mut client = match spawned {
            Ok(client) => client,
            Err(error) => {
                if let Some(path) = &ready_path {
                    let _ = std::fs::remove_file(path);
                }
                return Err(error);
            }
        };
        if let Some(path) = ready_path {
            let result = client.worker.wait_for_ready_file(&path);
            let _ = std::fs::remove_file(path);
            result?;
        }
        client.expects_strategy_checkpoint = runtime_delivery;
        Ok(client)
    }

    pub(crate) fn request(
        &mut self,
        input: &StrategyContractInput,
    ) -> Result<StrategyContractOutput, String> {
        self.last_strategy_checkpoint = None;
        let payload = input.to_json()?;
        let columnar_payload = if self.transport == StrategyTransport::SharedMemoryColumnar {
            Some(encode_strategy_columnar_input(input)?)
        } else {
            None
        };
        let line = self
            .worker
            .request(payload, columnar_payload)
            .map_err(|error| format!("{error}{}", self.worker.diagnostics()))?;
        let (output, checkpoint) = if self.expects_strategy_checkpoint {
            StrategyContractOutput::from_strategy_process_response_with_checkpoint(&line, input)
                .map_err(|error| format!("{error}{}", self.worker.diagnostics()))?
        } else {
            (
                StrategyContractOutput::from_python_response(&line, input)
                    .map_err(|error| format!("{error}{}", self.worker.diagnostics()))?,
                None,
            )
        };
        if self.expects_strategy_checkpoint && checkpoint.is_none() {
            return Err(format!(
                "Strategy process worker 响应缺少 durable checkpoint{}",
                self.worker.diagnostics()
            ));
        }
        self.last_strategy_checkpoint = checkpoint;
        Ok(output)
    }

    pub(crate) fn take_strategy_checkpoint(
        &mut self,
    ) -> Option<qx_strategy::contract::StrategyCheckpointV1> {
        self.last_strategy_checkpoint.take()
    }

    pub(crate) fn require_strategy_checkpoint(&mut self) {
        self.expects_strategy_checkpoint = true;
    }

    pub(crate) fn expects_strategy_checkpoint(&self) -> bool {
        self.expects_strategy_checkpoint
    }
}

fn frozen_python_dependency_root(module: &str) -> Option<PathBuf> {
    let entry = Path::new(module);
    if !entry.is_absolute() || !entry.is_file() {
        return None;
    }
    entry.ancestors().find_map(|ancestor| {
        (ancestor
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "strategy-deployment-trees"))
        .then(|| ancestor.to_path_buf())
    })
}

pub(crate) fn python_module_search_path() -> Result<std::ffi::OsString, String> {
    let python_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("python");
    let inherited_path = std::env::var("PYTHONPATH").unwrap_or_default();
    if inherited_path.trim().is_empty() {
        Ok(python_root.into_os_string())
    } else {
        let mut paths =
            std::env::split_paths(&std::ffi::OsString::from(inherited_path)).collect::<Vec<_>>();
        paths.insert(0, python_root);
        std::env::join_paths(paths)
            .map_err(|error| format!("构造 Python 模块搜索路径失败: {error}"))
    }
}

/// 冷启动一次 Python 策略 worker 并立即回收。`timeout_ms` 必须走配置
/// （`strategy.python_timeout_ms`，已按 1..=60000 校验）：这里若写死 `PYTHON_STRATEGY_TIMEOUT_MS`，
/// 预热分支（`workers.rs` / `strategy_contract.rs` 都读配置）拿到 30s、冷启动分支拿到 2s，
/// 同一个 `python_timeout_ms: 30000` 在两条路径上语义不同，用户配了 30s 却在首轮被 2s 掐死。
pub(crate) fn invoke_python_strategy(
    module: &str,
    input: &StrategyContractInput,
    timeout_ms: u64,
) -> Result<StrategyContractOutput, String> {
    let mut client = PythonStrategyClient::start(module, timeout_ms)?;
    client.request(input)
}

pub(crate) fn invoke_python_strategy_with_client(
    client: &mut PythonStrategyClient,
    input: &StrategyContractInput,
) -> Result<StrategyContractOutput, String> {
    client.request(input)
}
