//! 跨语言策略宿主：Python 子进程、受信任 C ABI 动态库与内置策略的契约装配。
//!
//! 三种策略传输在这里收敛为同一份 `StrategyContractInput/Output`，
//! 使回测、Paper 与 Live worker 共享同一套指纹与超时约束。

mod imports;
use imports::*;
#[cfg(test)]
pub(crate) use qx_orchestrator::shared_ring_arguments;
mod strategy_client;
pub(crate) use strategy_client::ContractStrategyClient;
// Production reads strategy.python_timeout_ms; this constant only pins the test default.
#[cfg(test)]
pub(crate) const PYTHON_STRATEGY_TIMEOUT_MS: u64 = 2_000;

pub(crate) struct PythonStrategyClient {
    worker: qx_orchestrator::StrategyWorkerProcess,
    transport: StrategyTransport,
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
        )
    }

    fn start_with_transport_config_and_ring_directory(
        module: &str,
        timeout_ms: u64,
        transport: StrategyTransport,
        ring_config: SharedRingConfig,
        artifact_sha256: Option<&str>,
        ring_directory: Option<&Path>,
    ) -> Result<Self, String> {
        let (python, interpreter_origin) = python_interpreter_origin();
        let python_path = python_module_search_path()?;
        let mut env = BTreeMap::new();
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
        let mut args = vec![
            "-m".into(),
            "qianxing_strategy.worker".into(),
            "--module".into(),
            module.into(),
        ];
        if transport == StrategyTransport::FramedJson {
            args.push("--protocol".into());
            args.push("framed_json".into());
        }
        Self::start_process_with_ring_directory(
            &python,
            &args,
            &env,
            timeout_ms,
            "Python Strategy",
            transport,
            ring_config,
            Some(interpreter_origin),
            ring_directory,
        )
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
        // 失败信息里的程序名：Python 路径还要带上解释器来源，因为"PATH 上的 python 是占位桩"
        // 与"策略代码报错"在协议层看起来一模一样（子进程一个字都没输出就退出）。
        let diagnostic_program = match interpreter_origin {
            Some(origin) => format!("{executable}（{origin}）"),
            None => executable.to_string(),
        };
        let worker = qx_orchestrator::StrategyWorkerProcess::spawn(
            qx_orchestrator::StrategyWorkerSpawnConfig {
                executable,
                args,
                env,
                timeout_ms,
                label,
                diagnostic_program: &diagnostic_program,
                transport,
                ring_config,
                ring_directory,
            },
        )?;
        Ok(Self { worker, transport })
    }

    pub(crate) fn request(
        &mut self,
        input: &StrategyContractInput,
    ) -> Result<StrategyContractOutput, String> {
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
        StrategyContractOutput::from_python_response(&line, input)
            .map_err(|error| format!("{error}{}", self.worker.diagnostics()))
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
    qx_app::build_strategy_context(qx_app::StrategyContextBindings {
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
    .map_err(|error| error.to_string())
}

pub(crate) fn invoke_c_abi_strategy(
    strategy: &mut DynamicCAbiStrategy,
    initialized: &mut bool,
    context: &qx_strategy::StrategyContext,
    input: &StrategyContractInput,
    event: &NativeMarketEvent,
) -> Result<StrategyContractOutput, String> {
    context.validate()?;
    event.validate()?;
    if !*initialized {
        strategy.on_init(context)?;
        *initialized = true;
    }
    let decision = strategy.on_event(context, event)?;
    native_decision_to_contract_output(&decision, input)
}

pub(crate) fn invoke_builtin_strategy(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    request_id: &str,
    now: u64,
) -> Result<StrategyContractOutput, String> {
    let input = build_strategy_contract_input(root, config, instrument, request_id, now)?;
    let bars = input.bars.as_ref().ok_or_else(|| {
        "builtin_strategy 运行时需要 bars_snapshot_path 提供 K 线历史".to_string()
    })?;
    let context = native_strategy_context(&config.strategy, &input)?;
    let mut strategy = BuiltinStrategy::new(qx_runtime::builtin_strategy_config_from_runtime(
        &config.strategy,
        instrument,
    )?)?;
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
        };
        decision = Some(strategy.on_event(&context, &event)?);
    }
    let decision = decision.ok_or_else(|| "builtin_strategy 可见 K 线为空".to_string())?;
    native_decision_to_contract_output(&decision, &input)
}
