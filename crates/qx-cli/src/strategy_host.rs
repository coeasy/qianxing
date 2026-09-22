//! 跨语言策略宿主：Python 子进程、受信任 C ABI 动态库与内置策略的契约装配。
//!
//! 三种策略传输在这里收敛为同一份 `StrategyContractInput/Output`，
//! 使回测、Paper 与 Live worker 共享同一套指纹与超时约束。

use super::*;
pub(crate) const PYTHON_STRATEGY_TIMEOUT_MS: u64 = 2_000;

pub(crate) enum StrategyWireResponse {
    JsonLine(String),
    Frame(StrategyFrame),
}

pub(crate) struct PythonStrategyClient {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    responses: Option<Receiver<Result<StrategyWireResponse, String>>>,
    shared_input: Option<SharedRingWriter>,
    shared_output: Option<SharedRingReader>,
    ring_paths: Option<(PathBuf, PathBuf)>,
    timeout_ms: u64,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    label: String,
    diagnostic_program: String,
    transport: StrategyTransport,
    next_sequence: u64,
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
        Self::start_process_with_transport_config(
            &python,
            &args,
            &env,
            timeout_ms,
            "Python Strategy",
            transport,
            ring_config,
            Some(interpreter_origin),
        )
    }

    // 进程启动参数天然就是"可执行 + 实参 + 环境 + 协议 + 诊断"五组，拆结构体只会把一次
    // spawn 的字段散到两处；与仓库其余 4 处同类启动函数保持同一约定。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_process_with_transport_config(
        executable: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        timeout_ms: u64,
        label: &str,
        transport: StrategyTransport,
        ring_config: SharedRingConfig,
        interpreter_origin: Option<&str>,
    ) -> Result<Self, String> {
        // 失败信息里的程序名：Python 路径还要带上解释器来源，因为"PATH 上的 python 是占位桩"
        // 与"策略代码报错"在协议层看起来一模一样（子进程一个字都没输出就退出）。
        let diagnostic_program = match interpreter_origin {
            Some(origin) => format!("{executable}（{origin}）"),
            None => executable.to_string(),
        };
        if matches!(
            transport,
            StrategyTransport::SharedMemoryJson | StrategyTransport::SharedMemoryColumnar
        ) {
            ring_config
                .validate()
                .map_err(|error| format!("{label} 共享 ring 配置非法: {error}"))?;
        }
        let mut actual_args = args.to_vec();
        let mut shared_input = None;
        let mut shared_output = None;
        let mut ring_paths = None;
        if matches!(
            transport,
            StrategyTransport::SharedMemoryJson | StrategyTransport::SharedMemoryColumnar
        ) {
            let ring_id = NEXT_STRATEGY_RING_ID.fetch_add(1, Ordering::Relaxed);
            let base = std::env::temp_dir().join(format!(
                "qianxing-strategy-ring-{}-{ring_id}",
                std::process::id()
            ));
            let input_path = base.with_extension("input");
            let output_path = base.with_extension("output");
            let input = SharedRingWriter::create(&input_path, ring_config)
                .map_err(|error| format!("创建 {label} 输入 ring 失败: {error}"))?;
            let output = SharedRingWriter::create(&output_path, ring_config)
                .map_err(|error| format!("创建 {label} 输出 ring 失败: {error}"))?;
            drop(output);
            let reader = SharedRingReader::open(&output_path, ring_config)
                .map_err(|error| format!("打开 {label} 输出 ring 失败: {error}"))?;
            actual_args.extend([
                "--protocol".into(),
                if transport == StrategyTransport::SharedMemoryColumnar {
                    "shared_memory_columnar".into()
                } else {
                    "shared_memory_json".into()
                },
                "--input-ring".into(),
                input_path.to_string_lossy().into_owned(),
                "--output-ring".into(),
                output_path.to_string_lossy().into_owned(),
                "--ring-capacity".into(),
                ring_config.capacity.to_string(),
                "--ring-slot-bytes".into(),
                ring_config.slot_bytes.to_string(),
            ]);
            shared_input = Some(input);
            shared_output = Some(reader);
            ring_paths = Some((input_path, output_path));
        }
        let child_env = strategy_child_environment(env)?;
        let mut command = Command::new(executable);
        command
            .args(&actual_args)
            // 策略进程只能获得最小运行时环境，不能继承 API secret 等父进程变量。
            .env_clear()
            .envs(child_env);
        let shared = matches!(
            transport,
            StrategyTransport::SharedMemoryJson | StrategyTransport::SharedMemoryColumnar
        );
        let mut child = command
            .stdin(if shared {
                Stdio::null()
            } else {
                Stdio::piped()
            })
            .stdout(if shared {
                Stdio::null()
            } else {
                Stdio::piped()
            })
            // Strategy stdout is the protocol; user diagnostics must use stderr.
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                format!("启动 {label} worker 失败: {diagnostic_program} 无法执行: {error}")
            })?;
        let stdin = if shared { None } else { child.stdin.take() };
        let stdout = if shared { None } else { child.stdout.take() };
        if !shared && (stdin.is_none() || stdout.is_none()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{label} worker stdin/stdout 不可用"));
        }
        let stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{label} worker stderr 不可用"));
            }
        };
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_tail_reader = Arc::clone(&stderr_tail);
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                if let Ok(mut tail) = stderr_tail_reader.lock() {
                    tail.push_back(line.chars().take(512).collect());
                    while tail.len() > 16 {
                        tail.pop_front();
                    }
                }
            }
        });
        let responses = if shared {
            None
        } else {
            let stdout = stdout.expect("non-shared worker stdout checked above");
            let (sender, responses) = mpsc::channel();
            thread::spawn(move || match transport {
                StrategyTransport::Jsonl => {
                    let reader = BufReader::new(stdout);
                    for line in reader.lines() {
                        match line {
                            Ok(line) => {
                                if sender
                                    .send(Ok(StrategyWireResponse::JsonLine(line)))
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            Err(error) => {
                                let _ = sender
                                    .send(Err(format!("读取 Strategy worker 响应失败: {error}")));
                                return;
                            }
                        }
                    }
                    let _ = sender.send(Err("Strategy worker 已关闭输出".into()));
                }
                StrategyTransport::FramedJson => {
                    let mut reader = stdout;
                    loop {
                        match StrategyFrame::read_from(&mut reader, DEFAULT_MAX_FRAME_BYTES) {
                            Ok(frame) => {
                                if sender.send(Ok(StrategyWireResponse::Frame(frame))).is_err() {
                                    return;
                                }
                            }
                            Err(error) => {
                                let _ = sender.send(Err(format!(
                                    "读取 Strategy worker 分帧响应失败: {error}"
                                )));
                                return;
                            }
                        }
                    }
                }
                StrategyTransport::SharedMemoryJson | StrategyTransport::SharedMemoryColumnar => {
                    unreachable!("shared transport has no pipe")
                }
            });
            Some(responses)
        };
        Ok(Self {
            child,
            stdin,
            responses,
            shared_input,
            shared_output,
            ring_paths,
            timeout_ms,
            stderr_tail,
            label: label.to_string(),
            diagnostic_program,
            transport,
            next_sequence: 1,
        })
    }

    fn diagnostics(&self) -> String {
        let Ok(tail) = self.stderr_tail.lock() else {
            return String::new();
        };
        if tail.is_empty() {
            String::new()
        } else {
            format!(
                "; stderr={}",
                tail.iter().cloned().collect::<Vec<_>>().join(" | ")
            )
        }
    }

    /// 子进程"一个字都没输出就结束"时的可判定信息：程序名与解释器来源、退出码、stderr 尾部。
    /// 少了这些，"PATH 上的 python 是 WindowsApps 占位桩"与"策略代码抛异常"在协议层是同一句话。
    fn death_note(&mut self) -> String {
        let exit = match self.child.try_wait() {
            Ok(Some(status)) => match status.code() {
                Some(code) => format!("退出码 {code}"),
                None => "无退出码（信号终止）".to_string(),
            },
            Ok(None) => "进程未退出".to_string(),
            Err(error) => format!("退出码不可读: {error}"),
        };
        let program = self.diagnostic_program.clone();
        let tail = self.diagnostics();
        if tail.is_empty() {
            return format!(
                "（程序={program}，{exit}，worker 无 stderr 输出；若该程序是 WindowsApps 的 python 占位桩，请把 QX_PYTHON 指向可用解释器）"
            );
        }
        format!("（程序={program}，{exit}{tail}）")
    }

    pub(crate) fn request(
        &mut self,
        input: &StrategyContractInput,
    ) -> Result<StrategyContractOutput, String> {
        let payload = input.to_json()?;
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        let response = match self.transport {
            StrategyTransport::SharedMemoryJson | StrategyTransport::SharedMemoryColumnar => {
                let request_payload = if self.transport == StrategyTransport::SharedMemoryColumnar {
                    encode_strategy_columnar_input(input)?
                } else {
                    payload.into_bytes()
                };
                let frame = StrategyFrame::request(sequence, request_payload)
                    .encode(DEFAULT_MAX_FRAME_BYTES)
                    .map_err(|error| format!("编码 {} 共享分帧输入失败: {error}", self.label))?;
                let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
                let input_ring = self
                    .shared_input
                    .as_mut()
                    .ok_or_else(|| format!("{} 共享输入 ring 不可用", self.label))?;
                loop {
                    match input_ring.try_push(&frame) {
                        Ok(()) => break,
                        Err(SharedRingError::Full) if Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => {
                            return Err(format!("写入 {} 共享 ring 失败: {error}", self.label));
                        }
                    }
                }
                let output_ring = self
                    .shared_output
                    .as_mut()
                    .ok_or_else(|| format!("{} 共享输出 ring 不可用", self.label))?;
                loop {
                    match output_ring.try_pop_frame() {
                        Ok(frame) => break StrategyWireResponse::Frame(frame),
                        Err(SharedRingError::Empty) if Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(SharedRingError::Empty) => {
                            let note = self.death_note();
                            let _ = self.child.kill();
                            return Err(format!(
                                "{} worker 响应超时 timeout_ms={}{}",
                                self.label, self.timeout_ms, note
                            ));
                        }
                        Err(error) => {
                            return Err(format!("读取 {} 共享 ring 失败: {error}", self.label));
                        }
                    }
                }
            }
            StrategyTransport::Jsonl | StrategyTransport::FramedJson => {
                let label = self.label.clone();
                let diagnostics = self.diagnostics();
                let stdin = self
                    .stdin
                    .as_mut()
                    .ok_or_else(|| format!("{} worker stdin 不可用", label))?;
                if self.transport == StrategyTransport::Jsonl {
                    stdin
                        .write_all(format!("{payload}\n").as_bytes())
                        .map_err(|error| {
                            format!("写入 {} 输入失败: {error}{}", label, diagnostics)
                        })?;
                } else {
                    let frame = StrategyFrame::request(sequence, payload.into_bytes())
                        .encode(DEFAULT_MAX_FRAME_BYTES)
                        .map_err(|error| format!("编码 {} 分帧输入失败: {error}", self.label))?;
                    stdin.write_all(&frame).map_err(|error| {
                        format!("写入 {} 分帧输入失败: {error}{}", label, diagnostics)
                    })?;
                }
                stdin
                    .flush()
                    .map_err(|error| format!("刷新 {} 输入失败: {error}{}", label, diagnostics))?;
                let received = self
                    .responses
                    .as_ref()
                    .ok_or_else(|| format!("{} worker 响应通道不可用", self.label))?
                    .recv_timeout(Duration::from_millis(self.timeout_ms));
                match received {
                    Ok(Ok(response)) => response,
                    // 读线程送来的协议错误（典型场景是 worker 一个字都没输出就退出）此前被裸
                    // 解包冒泡，丢掉了已抓到的 stderr 与子进程状态，只剩一句"已关闭输出"。
                    Ok(Err(error)) => return Err(format!("{error}{}", self.death_note())),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let note = self.death_note();
                        let _ = self.child.kill();
                        return Err(format!(
                            "{} worker 响应超时 timeout_ms={}{}",
                            self.label, self.timeout_ms, note
                        ));
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        let note = self.death_note();
                        return Err(format!("{} worker 响应通道已断开{}", self.label, note));
                    }
                }
            }
        };
        let line = match response {
            StrategyWireResponse::JsonLine(line) => line,
            StrategyWireResponse::Frame(frame) => {
                if frame.sequence != sequence {
                    return Err(format!(
                        "{} worker 响应序号不匹配: expected={} actual={}",
                        self.label, sequence, frame.sequence
                    ));
                }
                if !matches!(
                    frame.kind,
                    StrategyFrameKind::Response | StrategyFrameKind::Error
                ) {
                    return Err(format!(
                        "{} worker 返回非响应分帧: {:?}",
                        self.label, frame.kind
                    ));
                }
                String::from_utf8(frame.payload)
                    .map_err(|error| format!("{} worker 响应不是 UTF-8: {error}", self.label))?
            }
        };
        decode_python_strategy_response(&line, input)
            .map_err(|error| format!("{error}{}", self.diagnostics()))
    }
}

pub(crate) fn strategy_child_environment(
    configured: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut environment = BTreeMap::new();
    // These variables are needed for executable lookup and the Windows/Python
    // runtime, but do not carry account credentials.
    for key in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Ok(value) = std::env::var(key) {
            environment.insert(key.to_string(), value);
        }
    }
    for (key, value) in configured {
        let upper = key.to_ascii_uppercase();
        if [
            "SECRET",
            "TOKEN",
            "PASSWORD",
            "API_KEY",
            "PRIVATE_KEY",
            "CREDENTIAL",
        ]
        .iter()
        .any(|marker| upper.contains(marker))
        {
            return Err(format!(
                "策略 worker 环境变量 {key} 可能包含交易凭证，已拒绝"
            ));
        }
        environment.insert(key.clone(), value.clone());
    }
    Ok(environment)
}

impl Drop for PythonStrategyClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.shared_input.take();
        self.shared_output.take();
        if let Some((input, output)) = self.ring_paths.take() {
            let _ = std::fs::remove_file(input);
            let _ = std::fs::remove_file(output);
        }
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

pub(crate) fn decode_python_strategy_response(
    line: &str,
    input: &StrategyContractInput,
) -> Result<StrategyContractOutput, String> {
    let response: serde_json::Value = serde_json::from_str(line)
        .map_err(|error| format!("Python Strategy 响应 JSON 无效: {error}"))?;
    if response.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(format!(
            "Python Strategy 拒绝请求: {}",
            response
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
        ));
    }
    let encoded = serde_json::to_string(
        response
            .get("output")
            .ok_or_else(|| "Python Strategy 响应缺少 output".to_string())?,
    )
    .map_err(|error| format!("编码 Python Strategy output 失败: {error}"))?;
    StrategyContractOutput::from_json_for(&encoded, input)
}

pub(crate) fn invoke_python_strategy(
    module: &str,
    input: &StrategyContractInput,
) -> Result<StrategyContractOutput, String> {
    let mut client = PythonStrategyClient::start(module, PYTHON_STRATEGY_TIMEOUT_MS)?;
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

pub(crate) fn native_strategy_context(
    strategy: &StrategyRuntimeConfig,
    input: &StrategyContractInput,
) -> qx_strategy::StrategyContext {
    qx_strategy::StrategyContext {
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
    }
}

pub(crate) fn strategy_contract_output_from_native_decision(
    decision: &qx_strategy::StrategyDecision,
    input: &StrategyContractInput,
) -> Result<StrategyContractOutput, String> {
    let mut output = StrategyContractOutput::from_native_decision(decision)?;
    output.request_id = input.request_id.clone();
    output.strategy_id = input.strategy_id.clone();
    if output.instrument.is_empty() {
        output.instrument = input.instrument.clone();
    }
    if output.intents.is_empty() {
        output.target_qty = input
            .positions
            .get(&input.instrument)
            .copied()
            .unwrap_or_default();
    }
    output.validate_for(input)?;
    Ok(output)
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
    strategy_contract_output_from_native_decision(&decision, input)
}

pub(crate) fn builtin_strategy_config_from_runtime(
    strategy: &StrategyRuntimeConfig,
    instrument: &InstrumentId,
) -> Result<BuiltinStrategyConfig, String> {
    let name = strategy
        .builtin_strategy
        .as_deref()
        .ok_or_else(|| "Strategy 未配置 builtin_strategy".to_string())?;
    let kind = BuiltinStrategyKind::parse(name)?;
    let strategy_id = strategy
        .id
        .clone()
        .unwrap_or_else(|| format!("builtin-{}", kind.name()));
    let quantity_raw = strategy
        .builtin_quantity
        .map(i128::from)
        .or_else(|| (strategy.target_qty != 0).then_some(strategy.target_qty.abs()))
        .unwrap_or(1);
    if quantity_raw <= 0 {
        return Err("builtin_quantity 必须为正整数".into());
    }
    let mut config = BuiltinStrategyConfig {
        kind,
        strategy_id,
        strategy_version: strategy.version.clone(),
        instrument: instrument.clone(),
        quantity: Quantity::from_raw(quantity_raw),
        fast_window: 5,
        slow_window: 20,
        period: 14,
        threshold_bps: 100,
        reference_instrument: None,
        primary_policy: None,
        reference_policy: None,
    };
    apply_builtin_signal_overrides(&mut config, strategy);
    if let Some(reference) = strategy.builtin_reference_instrument.as_deref() {
        config.reference_instrument = Some(
            InstrumentId::parse(reference)
                .ok_or_else(|| format!("builtin_reference_instrument 非法: {reference}"))?,
        );
    }
    if config.reference_instrument.is_some() {
        let primary_product = strategy.product.unwrap_or(TradingProduct::Spot);
        let primary_margin =
            strategy
                .margin_mode
                .unwrap_or(if primary_product == TradingProduct::Spot {
                    MarginMode::Cash
                } else {
                    MarginMode::Cross
                });
        let primary_position = strategy.position_mode.unwrap_or(PositionMode::OneWay);
        config.primary_policy = Some(OrderPolicy {
            reduce_only: false,
            position_side: PositionSide::Net,
            margin_mode: primary_margin,
            position_mode: primary_position,
            leverage: strategy.leverage.unwrap_or(1),
            post_only: false,
        });
        let reference_margin = strategy
            .builtin_reference_margin_mode
            .unwrap_or(MarginMode::Cash);
        let reference_position = strategy
            .builtin_reference_position_mode
            .unwrap_or(PositionMode::OneWay);
        config.reference_policy = Some(OrderPolicy {
            reduce_only: false,
            position_side: PositionSide::Net,
            margin_mode: reference_margin,
            position_mode: reference_position,
            leverage: strategy.builtin_reference_leverage.unwrap_or(1),
            post_only: false,
        });
    }
    config.validate()?;
    Ok(config)
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
    let context = native_strategy_context(&config.strategy, &input);
    let mut strategy = BuiltinStrategy::new(builtin_strategy_config_from_runtime(
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
    strategy_contract_output_from_native_decision(&decision, &input)
}

pub(crate) enum ContractStrategyClient {
    Process(Box<PythonStrategyClient>),
    Native(Box<DynamicCAbiStrategy>),
}
