use super::imports::*;
use super::python_client::PythonStrategyClient;
use std::collections::BTreeMap;
use std::path::Path;

impl PythonStrategyClient {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_process_with_ring_directory_and_capabilities(
        executable: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        timeout_ms: u64,
        label: &str,
        transport: StrategyTransport,
        ring_config: SharedRingConfig,
        capabilities: &qx_strategy::capabilities::StrategyHostCapabilitiesV1,
        required_capabilities: &[qx_strategy::capabilities::StrategyCapability],
    ) -> Result<Self, String> {
        Self::start_process_with_ring_directory_and_host_provider_env(
            executable,
            args,
            env,
            timeout_ms,
            label,
            transport,
            ring_config,
            None,
            None,
            &BTreeMap::new(),
            Some(capabilities),
            Some(required_capabilities),
        )
    }

    #[cfg(test)]
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
            None,
            None,
        )
    }

    pub(super) fn start_process_with_ring_directory_and_host_provider_env(
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
        host_capabilities: Option<&qx_strategy::capabilities::StrategyHostCapabilitiesV1>,
        host_required_capabilities: Option<&[qx_strategy::capabilities::StrategyCapability]>,
    ) -> Result<Self, String> {
        // 失败信息里的程序名：Python 路径还要带上解释器来源，因为"PATH 上的 python 是占位桩"
        // 与"策略代码报错"在协议层看起来一模一样（子进程一个字都没输出就退出）。
        let diagnostic_program = match interpreter_origin {
            Some(origin) => format!("{executable}（{origin}）"),
            None => executable.to_string(),
        };
        let mut worker_env = env.clone();
        let mut capability_ack_path = None;
        if let Some(capabilities) = host_capabilities {
            const CAPABILITIES_ENV: &str = "QX_STRATEGY_HOST_CAPABILITIES_JSON";
            const REQUIRED_ENV: &str = "QX_STRATEGY_REQUIRED_CAPABILITIES_JSON";
            const ACK_FILE_ENV: &str = "QX_STRATEGY_CAPABILITY_ACK_FILE";
            const PROTOCOL_ENV: &str = "QX_STRATEGY_CONTRACT_SCHEMA_VERSION";
            if [CAPABILITIES_ENV, REQUIRED_ENV, ACK_FILE_ENV, PROTOCOL_ENV]
                .iter()
                .any(|key| worker_env.contains_key(*key))
            {
                return Err(
                    "external strategy env must not override reserved host capability variables"
                        .to_string(),
                );
            }
            let required = host_required_capabilities.unwrap_or(&[]);
            capabilities.validate_required(required)?;
            let encoded = serde_json::to_string(capabilities)
                .map_err(|error| format!("serialize strategy host capabilities: {error}"))?;
            if encoded.len() > 16 * 1024 {
                return Err(
                    "strategy host capabilities exceed 16 KiB process environment limit".into(),
                );
            }
            worker_env.insert(CAPABILITIES_ENV.into(), encoded);
            worker_env.insert(
                PROTOCOL_ENV.into(),
                qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION.to_string(),
            );
            if !required.is_empty() {
                let required_json = serde_json::to_string(required).map_err(|error| {
                    format!("serialize required strategy capabilities: {error}")
                })?;
                worker_env.insert(REQUIRED_ENV.into(), required_json);
                let path = super::sandbox_audit_path(
                    &std::env::temp_dir(),
                    &format!("{label}-capability-ack"),
                )
                .with_extension("json");
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| {
                        format!("create capability acknowledgment directory: {error}")
                    })?;
                }
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .map_err(|error| format!("reserve capability acknowledgment file: {error}"))?;
                worker_env.insert(ACK_FILE_ENV.into(), path.to_string_lossy().into_owned());
                capability_ack_path = Some(path);
            }
        }
        let spawn_config = qx_orchestrator::StrategyWorkerSpawnConfig {
            executable,
            args,
            env: &worker_env,
            timeout_ms,
            label,
            diagnostic_program: &diagnostic_program,
            transport,
            ring_config,
            ring_directory,
        };
        let spawned = if host_provider_env.is_empty() {
            qx_orchestrator::StrategyWorkerProcess::spawn(spawn_config)
        } else {
            qx_orchestrator::StrategyWorkerProcess::spawn_with_host_provider_env(
                spawn_config,
                host_provider_env,
            )
        };
        let worker = match spawned {
            Ok(worker) => worker,
            Err(error) => {
                if let Some(path) = &capability_ack_path {
                    let _ = std::fs::remove_file(path);
                }
                return Err(error);
            }
        };
        let mut client = Self {
            worker,
            transport,
            expects_strategy_checkpoint: false,
            last_strategy_checkpoint: None,
        };
        if let Some(path) = capability_ack_path {
            let acknowledgment =
                client
                    .worker
                    .wait_for_ack_file(&path, 64 * 1024)
                    .and_then(|payload| {
                        let parsed: qx_strategy::capabilities::StrategyHostCapabilityAckV1 =
                            serde_json::from_slice(&payload).map_err(|error| {
                                format!(
                                "external Strategy capability acknowledgment is invalid: {error}"
                            )
                            })?;
                        let required = host_required_capabilities.unwrap_or(&[]);
                        host_capabilities
                            .ok_or_else(|| "host capability catalog disappeared".to_string())?
                            .validate_acknowledgment(
                                &parsed,
                                required,
                                qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
                            )?;
                        Ok(())
                    });
            let _ = std::fs::remove_file(&path);
            acknowledgment?;
        }
        Ok(client)
    }
}

#[cfg(test)]
mod capability_dispatch_tests {
    use super::*;

    fn capability_ack_script(host_id_override: Option<&str>) -> String {
        let override_line = host_id_override.map_or_else(String::new, |host_id| {
            format!("ack['host_id'] = {host_id:?}\n")
        });
        format!(
            "import json,os,time\nhost=json.loads(os.environ['QX_STRATEGY_HOST_CAPABILITIES_JSON'])\nrequired=json.loads(os.environ['QX_STRATEGY_REQUIRED_CAPABILITIES_JSON'])\nack={{'schema_version':1,'host_id':host['host_id'],'profile':host['profile'],'strategy_contract_schema_version':int(os.environ['QX_STRATEGY_CONTRACT_SCHEMA_VERSION']),'supported':host['supported'],'required_capabilities':required,'ready':True}}\n{override_line}with open(os.environ['QX_STRATEGY_CAPABILITY_ACK_FILE'],'w',encoding='utf-8') as stream: json.dump(ack,stream)\ntime.sleep(10)\n"
        )
    }

    fn start_external(
        script: &str,
        required: &[qx_strategy::capabilities::StrategyCapability],
    ) -> Result<PythonStrategyClient, String> {
        let host = qx_strategy::capabilities::StrategyHostCapabilitiesV1::local_external_process();
        PythonStrategyClient::start_process_with_ring_directory_and_capabilities(
            "python",
            &["-c".into(), script.into()],
            &std::collections::BTreeMap::new(),
            2_000,
            "R08 external capability test",
            StrategyTransport::Jsonl,
            SharedRingConfig::default(),
            &host,
            required,
        )
    }

    #[test]
    fn external_worker_acknowledges_constructed_host_catalog_before_ready() {
        use qx_strategy::capabilities::StrategyCapability::{Bar, BarHistory};

        let script = capability_ack_script(None);
        let client = start_external(&script, &[Bar, BarHistory]);
        let _client = client.unwrap_or_else(|error| {
            panic!("external process must accept the exact local host catalog: {error}")
        });
    }

    #[test]
    fn external_worker_rejects_requirements_before_launch_and_mismatched_dispatch_ack() {
        use qx_strategy::capabilities::StrategyCapability::{Bar, Timer};

        let unsupported = start_external(&capability_ack_script(None), &[Timer]);
        assert!(unsupported
            .err()
            .expect("unsupported Timer must fail before process construction")
            .contains("does not support"));

        let mismatched = start_external(&capability_ack_script(Some("wrong_host")), &[Bar]);
        assert!(mismatched
            .err()
            .expect("host identity mismatch must fail dispatch")
            .contains("does not match host contract"));
    }
}
