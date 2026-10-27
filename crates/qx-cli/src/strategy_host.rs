//! 跨语言策略宿主：Python 子进程、受信任 C ABI 动态库与内置策略的契约装配。
//!
//! 三种策略传输在这里收敛为同一份 `StrategyContractInput/Output`，
//! 使回测、Paper 与 Live worker 共享同一套指纹与超时约束。

mod execution_policy;
mod imports;
pub(crate) use execution_policy::SandboxExecutionPolicy;
use imports::*;
#[cfg(test)]
pub(crate) use qx_orchestrator::shared_ring_arguments;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
mod strategy_client;
pub(crate) use strategy_client::ContractStrategyClient;
// Production reads strategy.python_timeout_ms; this constant only pins the test default.
#[cfg(test)]
pub(crate) const PYTHON_STRATEGY_TIMEOUT_MS: u64 = 2_000;
static SANDBOX_AUDIT_PATH_SEQUENCE: AtomicU64 = AtomicU64::new(1);

pub(crate) fn sandbox_audit_path(root: &Path, host_worker_id: &str) -> PathBuf {
    let worker_digest = qx_strategy::sha256_hex(host_worker_id.as_bytes());
    let sequence = SANDBOX_AUDIT_PATH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    root.join("strategy-sandbox-audit").join(format!(
        "{}-{timestamp_ms}-{}-{sequence}.jsonl",
        &worker_digest[..16],
        std::process::id()
    ))
}

#[cfg(test)]
mod sandbox_audit_path_tests {
    use super::sandbox_audit_path;
    use std::path::Path;

    #[test]
    fn sandbox_audit_path_is_unique_and_does_not_expose_worker_id() {
        let first = sandbox_audit_path(Path::new("runtime-data"), "strategy-worker-secret-id");
        let second = sandbox_audit_path(Path::new("runtime-data"), "strategy-worker-secret-id");

        assert!(first.starts_with(Path::new("runtime-data/strategy-sandbox-audit")));
        assert_ne!(first, second);
        assert!(!first
            .to_string_lossy()
            .contains("strategy-worker-secret-id"));
    }
}

enum ExecutionCallback<'a> {
    OrderStatus(&'a qx_strategy::StrategyOrderStatusUpdate),
    Fill(&'a qx_strategy::StrategyFillUpdateV2),
    Account(&'a qx_strategy::contract::StrategyAccountUpdateV1),
}

impl ExecutionCallback<'_> {
    fn key(&self) -> (u64, &str, &str, u64, u8, u64) {
        match self {
            Self::OrderStatus(update) => (
                update.ts,
                &update.order.account_id,
                &update.order.instrument,
                update.event_seq,
                0,
                update.order.order_id,
            ),
            Self::Fill(update) => (
                update.ts,
                &update.order.account_id,
                &update.order.instrument,
                update.event_seq,
                1,
                update.order.order_id,
            ),
            Self::Account(update) => (update.ts, &update.account_id, "", update.event_seq, 2, 0),
        }
    }
}

fn sort_execution_callbacks(callbacks: &mut [ExecutionCallback<'_>]) {
    callbacks.sort_by(|left, right| left.key().cmp(&right.key()));
}

pub(crate) struct PythonStrategyClient {
    worker: qx_orchestrator::StrategyWorkerProcess,
    transport: StrategyTransport,
    expects_strategy_checkpoint: bool,
    last_strategy_checkpoint: Option<qx_strategy::contract::StrategyCheckpointV1>,
}

pub(crate) type StrategyProcessClient = PythonStrategyClient;

fn strategy_session_ttl(variable: &str, timeout_ms: u64) -> Result<u32, String> {
    let session_ttl = std::env::var(variable)
        .unwrap_or_else(|_| "3600".to_string())
        .parse::<u32>()
        .map_err(|_| format!("{variable} must be an integer"))?;
    let minimum_session_ttl = ((timeout_ms.saturating_add(999) / 1000) + 30)
        .max(60)
        .min(u32::MAX as u64) as u32;
    if !(minimum_session_ttl..=86_400).contains(&session_ttl) {
        return Err(format!(
            "{variable} must be from {minimum_session_ttl} through 86400"
        ));
    }
    Ok(session_ttl)
}

fn inherit_provider_sdk_environment(env: &mut BTreeMap<String, String>) {
    // Provider SDKs may rely on platform path and certificate settings. These stay
    // inside the local bridge process and are never copied into the remote command.
    for name in ["PATH", "SYSTEMROOT", "WINDIR", "TEMP", "TMP"] {
        if let Ok(value) = std::env::var(name) {
            env.insert(name.to_string(), value);
        }
    }
}

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
        )
    }

    #[cfg(test)]
    pub(crate) fn start_with_strategy_config_in(
        module: &str,
        timeout_ms: u64,
        transport: StrategyTransport,
        ring_config: SharedRingConfig,
        artifact_sha256: Option<&str>,
        strategy_spec: Option<&serde_json::Value>,
        account_id: Option<&str>,
        venue_id: Option<&str>,
        ring_directory: &Path,
    ) -> Result<Self, String> {
        Self::start_with_transport_config_and_ring_directory(
            module,
            timeout_ms,
            transport,
            ring_config,
            artifact_sha256,
            Some(ring_directory),
            strategy_spec,
            account_id,
            venue_id,
            None,
            None,
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
        if let Some(artifact_sha256) = artifact_sha256 {
            env.insert(
                "QX_STRATEGY_ARTIFACT_SHA256".to_string(),
                artifact_sha256.to_string(),
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
        let worker_module = match execution_mode {
            "local" => "qianxing_strategy.worker",
            "e2b" => {
                let template = std::env::var("QX_E2B_TEMPLATE")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| "E2B mode requires host setting QX_E2B_TEMPLATE".to_string())?;
                let api_key = std::env::var("E2B_API_KEY")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| "E2B mode requires host setting E2B_API_KEY".to_string())?;
                let session_ttl = strategy_session_ttl("QX_E2B_SESSION_TTL_SECONDS", timeout_ms)?;
                env.insert("QX_E2B_TEMPLATE".into(), template);
                host_provider_env.insert("E2B_API_KEY".into(), api_key);
                env.insert("QX_E2B_SESSION_TTL_SECONDS".into(), session_ttl.to_string());
                env.insert(
                    "QX_STRATEGY_PYTHON_TIMEOUT_MS".into(),
                    timeout_ms.to_string(),
                );
                inherit_provider_sdk_environment(&mut env);
                "qianxing_strategy.e2b_worker"
            }
            "llm-sandbox" => {
                let image = std::env::var("QX_LLMSANDBOX_IMAGE")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        "llm-sandbox mode requires host setting QX_LLMSANDBOX_IMAGE".to_string()
                    })?;
                let docker_host = std::env::var("QX_LLMSANDBOX_DOCKER_HOST")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        "llm-sandbox mode requires TLS remote Docker setting QX_LLMSANDBOX_DOCKER_HOST".to_string()
                    })?;
                let session_ttl =
                    strategy_session_ttl("QX_LLMSANDBOX_SESSION_TTL_SECONDS", timeout_ms)?;
                for (name, value) in [
                    ("QX_LLMSANDBOX_IMAGE", image),
                    ("QX_LLMSANDBOX_SESSION_TTL_SECONDS", session_ttl.to_string()),
                    (
                        "QX_LLMSANDBOX_EXPECTED_CPU_COUNT",
                        std::env::var("QX_LLMSANDBOX_EXPECTED_CPU_COUNT")
                            .unwrap_or_else(|_| "2".into()),
                    ),
                    (
                        "QX_LLMSANDBOX_EXPECTED_MEMORY_MB",
                        std::env::var("QX_LLMSANDBOX_EXPECTED_MEMORY_MB")
                            .unwrap_or_else(|_| "2048".into()),
                    ),
                    ("QX_STRATEGY_PYTHON_TIMEOUT_MS", timeout_ms.to_string()),
                ] {
                    env.insert(name.into(), value);
                }
                host_provider_env.insert("QX_LLMSANDBOX_DOCKER_HOST".into(), docker_host);
                for name in [
                    "QX_LLMSANDBOX_DOCKER_CA_CERT",
                    "QX_LLMSANDBOX_DOCKER_CLIENT_CERT",
                    "QX_LLMSANDBOX_DOCKER_CLIENT_KEY",
                ] {
                    if let Ok(value) = std::env::var(name) {
                        if !value.trim().is_empty() {
                            host_provider_env.insert(name.into(), value);
                        }
                    }
                }
                inherit_provider_sdk_environment(&mut env);
                "qianxing_strategy.llm_sandbox_worker"
            }
            "microsandbox" => {
                let image = std::env::var("QX_MICROSANDBOX_IMAGE")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        "Microsandbox mode requires host setting QX_MICROSANDBOX_IMAGE".to_string()
                    })?;
                let backend = std::env::var("QX_MICROSANDBOX_BACKEND")
                    .unwrap_or_else(|_| "local".to_string());
                if !matches!(backend.as_str(), "cloud" | "local") {
                    return Err("QX_MICROSANDBOX_BACKEND must be cloud or local".to_string());
                }
                let profile = std::env::var("QX_MICROSANDBOX_PROFILE")
                    .ok()
                    .filter(|value| !value.trim().is_empty());
                let api_key = std::env::var("MICROSANDBOX_API_KEY")
                    .ok()
                    .filter(|value| !value.trim().is_empty());
                if backend == "cloud" && profile.is_none() && api_key.is_none() {
                    return Err(
                        "Microsandbox cloud mode requires host setting MICROSANDBOX_API_KEY or QX_MICROSANDBOX_PROFILE".to_string(),
                    );
                }
                let session_ttl =
                    strategy_session_ttl("QX_MICROSANDBOX_SESSION_TTL_SECONDS", timeout_ms)?;
                env.insert("QX_MICROSANDBOX_IMAGE".into(), image);
                env.insert("QX_MICROSANDBOX_BACKEND".into(), backend.clone());
                env.insert(
                    "QX_MICROSANDBOX_SESSION_TTL_SECONDS".into(),
                    session_ttl.to_string(),
                );
                env.insert(
                    "QX_STRATEGY_PYTHON_TIMEOUT_MS".into(),
                    timeout_ms.to_string(),
                );
                if let Some(profile) = &profile {
                    env.insert("QX_MICROSANDBOX_PROFILE".into(), profile.clone());
                    for name in ["HOME", "USERPROFILE"] {
                        if let Ok(value) = std::env::var(name) {
                            env.insert(name.into(), value);
                        }
                    }
                }
                if let Ok(url) = std::env::var("QX_MICROSANDBOX_URL") {
                    if !url.trim().is_empty() {
                        env.insert("QX_MICROSANDBOX_URL".into(), url);
                    }
                }
                for name in ["QX_MICROSANDBOX_CPUS", "QX_MICROSANDBOX_MEMORY_MIB"] {
                    if let Ok(value) = std::env::var(name) {
                        env.insert(name.into(), value);
                    }
                }
                if backend == "cloud" {
                    if profile.is_none() {
                        if let Some(api_key) = api_key {
                            host_provider_env.insert("MICROSANDBOX_API_KEY".into(), api_key);
                        }
                    }
                }
                inherit_provider_sdk_environment(&mut env);
                "qianxing_strategy.microsandbox_worker"
            }
            "opensandbox" => {
                let domain = std::env::var("OPEN_SANDBOX_DOMAIN")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        "OpenSandbox mode requires host setting OPEN_SANDBOX_DOMAIN".to_string()
                    })?;
                let image = std::env::var("QX_OPENSANDBOX_IMAGE")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        "OpenSandbox mode requires host setting QX_OPENSANDBOX_IMAGE".to_string()
                    })?;
                let session_ttl =
                    strategy_session_ttl("QX_OPENSANDBOX_SESSION_TTL_SECONDS", timeout_ms)?;
                env.insert("OPEN_SANDBOX_DOMAIN".into(), domain);
                env.insert("QX_OPENSANDBOX_IMAGE".into(), image);
                env.insert(
                    "QX_OPENSANDBOX_SESSION_TTL_SECONDS".into(),
                    session_ttl.to_string(),
                );
                env.insert(
                    "QX_STRATEGY_PYTHON_TIMEOUT_MS".into(),
                    timeout_ms.to_string(),
                );
                if let Ok(api_key) = std::env::var("OPEN_SANDBOX_API_KEY") {
                    if !api_key.trim().is_empty() {
                        host_provider_env.insert("OPEN_SANDBOX_API_KEY".into(), api_key);
                    }
                }
                for name in ["QX_OPENSANDBOX_CPUS", "QX_OPENSANDBOX_MEMORY_MIB"] {
                    if let Ok(value) = std::env::var(name) {
                        env.insert(name.into(), value);
                    }
                }
                inherit_provider_sdk_environment(&mut env);
                "qianxing_strategy.opensandbox_worker"
            }
            _ => unreachable!("execution mode was validated above"),
        };
        let mut args = vec![
            "-m".into(),
            worker_module.into(),
            "--module".into(),
            module.into(),
        ];
        if transport == StrategyTransport::FramedJson {
            args.push("--protocol".into());
            args.push("framed_json".into());
        }
        // Remote startup includes sandbox creation, source/data transfer and a
        // package preflight. Give that one-time bridge startup a bounded budget
        // beyond the strategy's per-request timeout; each provider bridge still
        // enforces the configured timeout for every strategy response.
        let worker_timeout_ms = if remote_execution {
            timeout_ms.saturating_add(240_000)
        } else {
            timeout_ms
        };
        let mut client = Self::start_process_with_ring_directory_and_host_provider_env(
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
        )?;
        client.expects_strategy_checkpoint = strategy_spec.is_some();
        Ok(client)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_process_with_ring_directory(
        executable: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        timeout_ms: u64,
        label: &str,
        transport: StrategyTransport,
        ring_config: SharedRingConfig,
        interpreter_origin: Option<&str>,
        ring_directory: Option<&Path>,
    ) -> Result<Self, String> {
        Self::start_process_with_ring_directory_and_host_provider_env(
            executable,
            args,
            env,
            timeout_ms,
            label,
            transport,
            ring_config,
            interpreter_origin,
            ring_directory,
            &BTreeMap::new(),
        )
    }

    fn start_process_with_ring_directory_and_host_provider_env(
        executable: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        timeout_ms: u64,
        label: &str,
        transport: StrategyTransport,
        ring_config: SharedRingConfig,
        interpreter_origin: Option<&str>,
        ring_directory: Option<&Path>,
        host_provider_env: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        // 失败信息里的程序名：Python 路径还要带上解释器来源，因为"PATH 上的 python 是占位桩"
        // 与"策略代码报错"在协议层看起来一模一样（子进程一个字都没输出就退出）。
        let diagnostic_program = match interpreter_origin {
            Some(origin) => format!("{executable}（{origin}）"),
            None => executable.to_string(),
        };
        let spawn_config = qx_orchestrator::StrategyWorkerSpawnConfig {
            executable,
            args,
            env,
            timeout_ms,
            label,
            diagnostic_program: &diagnostic_program,
            transport,
            ring_config,
            ring_directory,
        };
        let worker = if host_provider_env.is_empty() {
            qx_orchestrator::StrategyWorkerProcess::spawn(spawn_config)?
        } else {
            qx_orchestrator::StrategyWorkerProcess::spawn_with_host_provider_env(
                spawn_config,
                host_provider_env,
            )?
        };
        Ok(Self {
            worker,
            transport,
            expects_strategy_checkpoint: false,
            last_strategy_checkpoint: None,
        })
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

pub(crate) fn load_c_abi_strategy(
    strategy: &StrategyRuntimeConfig,
) -> Result<DynamicCAbiStrategy, String> {
    // P0-3 / DD-4：`dlopen` 之前先过信任门（trusted_native + 签名信任根 + 架构/平台匹配）。
    native_trust::admit_in_process_c_abi(strategy)?;
    let library = strategy
        .c_abi_library
        .as_deref()
        .ok_or_else(|| "C ABI 策略缺少 c_abi_library".to_string())?;
    let expected_sha256 = strategy
        .c_abi_sha256
        .as_deref()
        .ok_or_else(|| "C ABI 策略缺少 c_abi_sha256".to_string())?;
    let mut policy = DynamicCAbiLoadPolicy::new(expected_sha256)
        .with_max_library_bytes(strategy.c_abi_max_library_bytes);
    if let (Some(public_key), Some(signature)) = (
        strategy.c_abi_ed25519_public_key.as_deref(),
        strategy.c_abi_ed25519_signature.as_deref(),
    ) {
        policy = policy.with_ed25519_signature(public_key, signature);
    }
    let config_json = serde_json::to_string(strategy)
        .map_err(|error| format!("C ABI 策略配置编码失败: {error}"))?;
    unsafe { DynamicCAbiStrategy::load_verified(library, &config_json, &policy) }
        .map_err(|error| format!("加载 C ABI 策略失败: {error}"))
}

/// Adapt resolved runtime fields to the application-owned native strategy context.
pub(crate) fn native_strategy_context(
    strategy: &StrategyRuntimeConfig,
    input: &StrategyContractInput,
) -> Result<qx_strategy::StrategyContext, String> {
    let mut context = qx_app::build_strategy_context(qx_app::StrategyContextBindings {
        strategy_id: input.strategy_id.clone(),
        strategy_version: input.strategy_version.clone(),
        account_id: strategy
            .account_id
            .clone()
            .unwrap_or_else(|| "runtime".into()),
        venue_id: strategy
            .venue_id
            .clone()
            .unwrap_or_else(|| "runtime".into()),
        data_fingerprint: input.data_fingerprint.clone(),
        as_of: input.as_of,
        positions: input.positions.clone(),
        cash: input.cash.clone(),
        available_margin_raw: input.available_margin_raw,
        risk_state: input.risk_state.clone(),
    })
    .map_err(|error| error.to_string())?;
    context.positions_exact = input
        .positions_exact
        .iter()
        .map(|(instrument, amount)| {
            qx_core::ExactDecimal::parse(amount)
                .map(|amount| (instrument.clone(), amount))
                .map_err(|error| {
                    format!("StrategyContractInput positions_exact[{instrument}] 非法: {error}")
                })
        })
        .collect::<Result<_, _>>()?;
    context.cash_exact = input
        .cash_exact
        .iter()
        .map(|(currency, amount)| {
            qx_core::ExactDecimal::parse(amount)
                .map(|amount| (currency.clone(), amount))
                .map_err(|error| {
                    format!("StrategyContractInput cash_exact[{currency}] 非法: {error}")
                })
        })
        .collect::<Result<_, _>>()?;
    context.available_margin_exact = input
        .available_margin_exact
        .as_deref()
        .map(qx_core::ExactDecimal::parse)
        .transpose()
        .map_err(|error| format!("StrategyContractInput available_margin_exact 非法: {error}"))?;
    context.validate()?;
    Ok(context)
}

pub(crate) fn invoke_c_abi_strategy(
    strategy: &mut DynamicCAbiStrategy,
    initialized: &mut bool,
    context: &qx_strategy::StrategyContext,
    input: &StrategyContractInput,
    event: &NativeMarketEvent,
) -> Result<StrategyContractOutput, String> {
    invoke_c_abi_strategy_with_updates(
        strategy,
        initialized,
        context,
        input,
        event,
        &input.execution_updates,
    )
}

pub(crate) fn invoke_c_abi_strategy_with_updates(
    strategy: &mut DynamicCAbiStrategy,
    initialized: &mut bool,
    context: &qx_strategy::StrategyContext,
    input: &StrategyContractInput,
    event: &NativeMarketEvent,
    execution_updates: &[qx_strategy::StrategyOrderStatusUpdate],
) -> Result<StrategyContractOutput, String> {
    invoke_c_abi_strategy_with_execution_updates(
        strategy,
        initialized,
        context,
        input,
        event,
        execution_updates,
        &input.fill_updates,
        &input.account_updates,
    )
}

pub(crate) fn invoke_c_abi_strategy_with_execution_updates(
    strategy: &mut DynamicCAbiStrategy,
    initialized: &mut bool,
    context: &qx_strategy::StrategyContext,
    input: &StrategyContractInput,
    event: &NativeMarketEvent,
    execution_updates: &[qx_strategy::StrategyOrderStatusUpdate],
    fill_updates: &[qx_strategy::StrategyFillUpdateV2],
    account_updates: &[qx_strategy::contract::StrategyAccountUpdateV1],
) -> Result<StrategyContractOutput, String> {
    context.validate()?;
    event.validate()?;
    if strategy.has_partial_state_checkpoint_exports() {
        return Err("C ABI Strategy checkpoint snapshot/restore 导出必须成对提供".into());
    }
    for update in execution_updates {
        update.validate_for(context)?;
    }
    for update in fill_updates {
        update.validate_for(context)?;
    }
    for update in account_updates {
        update.validate_for(context)?;
    }
    if !*initialized {
        if let Err(message) = strategy.on_init(context) {
            return Err(notify_c_abi_strategy_error(
                strategy,
                context,
                StrategyErrorStage::Initialization,
                Some(context.as_of),
                None,
                message,
            ));
        }
        *initialized = true;
    }

    if strategy.supports_state_checkpoint() {
        let checkpoint = input
            .strategy_checkpoint
            .as_ref()
            .ok_or_else(|| "C ABI Strategy durable checkpoint 缺失，拒绝无状态恢复".to_string())?;
        checkpoint.validate_for_input(input)?;
        if checkpoint
            .adapter_state
            .get("state_mode")
            .and_then(|value| value.as_str())
            != Some("c-abi")
        {
            return Err("C ABI Strategy checkpoint adapter_state 不匹配".into());
        }
        strategy.restore_strategy_checkpoint(&checkpoint.strategy_state)?;
    }

    let mut callbacks = execution_updates
        .iter()
        .filter(|_| strategy.supports_order_status_updates())
        .map(ExecutionCallback::OrderStatus)
        .chain(
            fill_updates
                .iter()
                .filter(|_| strategy.supports_fill_updates())
                .map(ExecutionCallback::Fill),
        )
        .chain(
            account_updates
                .iter()
                .filter(|_| strategy.supports_account_updates())
                .map(ExecutionCallback::Account),
        )
        .collect::<Vec<_>>();
    sort_execution_callbacks(&mut callbacks);

    let mut decisions = Vec::new();
    for callback in callbacks {
        let (stage, timestamp, instrument_text, result) = match callback {
            ExecutionCallback::OrderStatus(update) => (
                StrategyErrorStage::OrderUpdate,
                update.ts,
                Some(update.order.instrument.as_str()),
                strategy.on_strategy_order_status(context, update),
            ),
            ExecutionCallback::Fill(update) => (
                StrategyErrorStage::FillUpdate,
                update.ts,
                Some(update.order.instrument.as_str()),
                strategy.on_strategy_fill_update_v2(context, update),
            ),
            ExecutionCallback::Account(update) => (
                StrategyErrorStage::AccountUpdate,
                update.ts,
                None,
                strategy.on_strategy_account_update(context, update),
            ),
        };
        let instrument = instrument_text.and_then(InstrumentId::parse);
        let decision = match result {
            Ok(decision) => decision,
            Err(message) => {
                return Err(notify_c_abi_strategy_error(
                    strategy,
                    context,
                    stage,
                    Some(timestamp),
                    instrument.as_ref(),
                    message,
                ));
            }
        };
        if let Some(decision) = decision {
            decisions.push(decision);
        }
    }
    let mut decision = match strategy.on_event(context, event) {
        Ok(decision) => decision,
        Err(message) => {
            return Err(notify_c_abi_strategy_error(
                strategy,
                context,
                StrategyErrorStage::MarketEvent,
                Some(event.ts()),
                event.instrument(),
                message,
            ));
        }
    };
    let update_expiry = decisions
        .iter()
        .map(|update_decision| update_decision.expires_at)
        .filter(|expiry| *expiry > 0)
        .min();
    let mut ordered_intents = decisions
        .iter_mut()
        .flat_map(|update_decision| update_decision.intents.drain(..))
        .collect::<Vec<_>>();
    ordered_intents.append(&mut decision.intents);
    decision.intents = ordered_intents;
    decision.expires_at = match (decision.expires_at, update_expiry) {
        (0, Some(expiry)) => expiry,
        (expiry, None) => expiry,
        (left, Some(right)) => left.min(right),
    };
    native_decision_to_contract_output(&decision, input).map_err(|error| {
        notify_c_abi_strategy_error(
            strategy,
            context,
            StrategyErrorStage::MarketEvent,
            Some(event.ts()),
            event.instrument(),
            error.to_string(),
        )
    })
}

fn notify_c_abi_strategy_error(
    strategy: &mut DynamicCAbiStrategy,
    context: &qx_strategy::StrategyContext,
    stage: StrategyErrorStage,
    timestamp: Option<u64>,
    instrument: Option<&InstrumentId>,
    message: String,
) -> String {
    let error = StrategyErrorV1::new(
        stage,
        context,
        timestamp,
        instrument.cloned(),
        message.clone(),
    );
    qx_strategy::notify_strategy_error(strategy, &error);
    message
}

pub(crate) fn invoke_builtin_strategy(
    root: &Path,
    config: &RuntimeConfig,
    runtime_config_path: Option<&Path>,
    instrument: &InstrumentId,
    request_id: &str,
    now: u64,
) -> Result<StrategyContractOutput, String> {
    let input = build_strategy_contract_input(root, config, instrument, request_id, now)?;
    let bars = input.bars.as_ref().ok_or_else(|| {
        "builtin_strategy 运行时需要 bars_snapshot_path 提供 K 线历史".to_string()
    })?;
    let context = native_strategy_context(&config.strategy, &input)?;
    let mut builtin_config =
        qx_runtime::builtin_strategy_config_from_runtime(&config.strategy, instrument)?;
    let mut reference_route = None;
    if builtin_config.reference_instrument.is_some() {
        builtin_config.primary_market_spec =
            strategy_execution_market_spec(config, instrument, runtime_config_path)?;
        let reference = builtin_config
            .reference_instrument
            .as_ref()
            .ok_or("双腿内置策略丢失 reference_instrument")?;
        let reference_account_id = strategy_execution_account_for_instrument(
            config,
            config.strategy.account_id.as_deref(),
            reference,
        )?;
        builtin_config.reference_market_spec = strategy_execution_market_spec_for_account(
            config,
            &reference_account_id,
            reference,
            runtime_config_path,
        )?;
        reference_route = Some((reference.clone(), reference_account_id));
    }
    let mut strategy = BuiltinStrategy::new(builtin_config)?;
    strategy.on_init(&context)?;
    let reference_bars =
        if let Some(reference_text) = config.strategy.builtin_reference_instrument.as_deref() {
            let reference = InstrumentId::parse(reference_text)
                .ok_or_else(|| format!("builtin_reference_instrument 非法: {reference_text}"))?;
            let reference_path = config
                .strategy
                .builtin_reference_bars_snapshot_path
                .as_deref()
                .ok_or_else(|| "双腿套利缺少 builtin_reference_bars_snapshot_path".to_string())?;
            let mut reference_config = config.clone();
            reference_config.strategy.instrument = Some(reference.to_string());
            reference_config.strategy.bars_snapshot_path = Some(reference_path.into());
            load_strategy_contract_bars(root, &reference_config, &reference, now)?
                .map(|(bars, _, _)| (reference, bars))
        } else {
            None
        };
    let mut events = bars
        .ts
        .iter()
        .enumerate()
        .map(|(index, ts)| (*ts, false, index))
        .collect::<Vec<_>>();
    if let Some((_, reference)) = reference_bars.as_ref() {
        events.extend(
            reference
                .ts
                .iter()
                .enumerate()
                .map(|(index, ts)| (*ts, true, index)),
        );
    }
    events.sort_by_key(|(ts, is_reference, _)| (*ts, !*is_reference));
    let mut decision = None;
    for (_, is_reference, index) in events {
        let (event_instrument, event_bars) = if is_reference {
            let (reference, bars) = reference_bars.as_ref().ok_or("套利对冲腿 BarFrame 缺失")?;
            (reference.clone(), bars)
        } else {
            (instrument.clone(), bars)
        };
        let event = NativeMarketEvent::Bar {
            instrument: event_instrument,
            ts: event_bars.ts[index],
            open_raw: event_bars.open_raw[index],
            high_raw: event_bars.high_raw[index],
            low_raw: event_bars.low_raw[index],
            close_raw: event_bars.close_raw[index],
            volume_raw: event_bars.volume_raw[index],
            exact_ohlcv: None,
        };
        decision = Some(strategy.on_event(&context, &event)?);
    }
    let decision = decision.ok_or_else(|| "builtin_strategy 可见 K 线为空".to_string())?;
    let mut output = native_decision_to_contract_output(&decision, &input)?;
    if let Some((reference, reference_account_id)) = reference_route {
        if config.strategy.account_id.as_deref() != Some(reference_account_id.as_str()) {
            for intent in &mut output.intents {
                if InstrumentId::parse(&intent.instrument).as_ref() == Some(&reference) {
                    intent.account_id = Some(reference_account_id.clone());
                }
            }
        }
    }
    Ok(output)
}

#[cfg(test)]
mod execution_callback_order_tests {
    use super::{sort_execution_callbacks, ExecutionCallback};

    #[test]
    fn order_status_precedes_fill_for_the_same_event_log_sequence() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../qx-strategy/tests/fixtures/strategy-execution-sequence-v1.json"
        ))
        .unwrap();
        let events = fixture["events"].as_array().unwrap();
        let mut statuses = Vec::new();
        let mut fills = Vec::new();
        for event in events {
            match event["kind"].as_str().unwrap() {
                "order_status" => statuses.push(
                    serde_json::from_value::<qx_strategy::StrategyOrderStatusUpdate>(
                        event["update"].clone(),
                    )
                    .unwrap(),
                ),
                "fill" => fills.push(
                    serde_json::from_value::<qx_strategy::StrategyFillUpdateV2>(
                        event["update"].clone(),
                    )
                    .unwrap(),
                ),
                kind => panic!("unexpected execution update kind: {kind}"),
            }
        }
        let mut callbacks = statuses
            .iter()
            .map(ExecutionCallback::OrderStatus)
            .chain(fills.iter().map(ExecutionCallback::Fill))
            .collect::<Vec<_>>();

        sort_execution_callbacks(&mut callbacks);

        let callback_order = callbacks
            .iter()
            .map(|callback| match callback {
                ExecutionCallback::OrderStatus(update) => format!(
                    "{}:{}:{}:order_status",
                    update.order.account_id, update.order.instrument, update.event_seq
                ),
                ExecutionCallback::Fill(update) => format!(
                    "{}:{}:{}:fill",
                    update.order.account_id, update.order.instrument, update.event_seq
                ),
                ExecutionCallback::Account(update) => format!(
                    "{}:{}:account:{}",
                    update.account_id, update.venue_id, update.event_seq
                ),
            })
            .collect::<Vec<_>>();
        let expected_callback_order = fixture["expected_callback_order"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(callback_order, expected_callback_order);
    }

    #[test]
    fn account_snapshot_is_ordered_with_order_and_fill_facts() {
        let account_update = qx_strategy::contract::StrategyAccountUpdateV1 {
            schema_version: 1,
            event: "account_snapshot".into(),
            event_seq: 22,
            ts: 10,
            account_id: "paper".into(),
            venue_id: "BINANCE".into(),
            balances: Some(Vec::new()),
            positions: None,
        };
        let order_update: qx_strategy::StrategyOrderStatusUpdate = serde_json::from_str(
            include_str!("../../qx-strategy/tests/fixtures/strategy-order-status-accepted-v1.json"),
        )
        .unwrap();
        let mut callbacks = vec![
            ExecutionCallback::OrderStatus(&order_update),
            ExecutionCallback::Account(&account_update),
        ];

        sort_execution_callbacks(&mut callbacks);

        assert!(matches!(callbacks[0], ExecutionCallback::Account(_)));
        assert!(matches!(callbacks[1], ExecutionCallback::OrderStatus(_)));
    }
}
