//! 策略边界：跨语言（Python/C-ABI/内置）策略客户端、目标仓位与订单构造。
//!
//! 策略只产出意图，订单仍需经过风控、OMS 与 Venue 端口。

use super::*;

static NEXT_STRATEGY_RING_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn resolve_strategy_runtime_paths(
    strategy: &mut StrategyRuntimeConfig,
    runtime_path: &Path,
) {
    if let Some(configured) = strategy.target_snapshot_path.as_deref() {
        strategy.target_snapshot_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(configured) = strategy.research_snapshot_path.as_deref() {
        strategy.research_snapshot_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(configured) = strategy.dataset_bundle_path.as_deref() {
        strategy.dataset_bundle_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    for configured in strategy.dataset_component_paths.values_mut() {
        *configured = resolve_runtime_relative_path(runtime_path, configured)
            .to_string_lossy()
            .into_owned();
    }
    if let Some(configured) = strategy.bars_snapshot_path.as_deref() {
        strategy.bars_snapshot_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(configured) = strategy.ashare_rules_path.as_deref() {
        strategy.ashare_rules_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(configured) = strategy.ashare_actions_path.as_deref() {
        strategy.ashare_actions_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(configured) = strategy.ashare_calendar_path.as_deref() {
        strategy.ashare_calendar_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(configured) = strategy.cost_rules_path.as_deref() {
        strategy.cost_rules_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(configured) = strategy.builtin_reference_bars_snapshot_path.as_deref() {
        strategy.builtin_reference_bars_snapshot_path = Some(
            resolve_runtime_relative_path(runtime_path, configured)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(executable) = strategy.external_executable.as_deref() {
        let path_like = executable.contains('/')
            || executable.contains('\\')
            || executable.starts_with('.')
            || Path::new(executable).is_absolute();
        if path_like {
            strategy.external_executable = Some(
                resolve_runtime_relative_path(runtime_path, executable)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    if let Some(module) = strategy.python_module.as_deref() {
        let path_like = module.ends_with(".py") || module.contains('/') || module.contains('\\');
        if path_like {
            strategy.python_module = Some(
                resolve_runtime_relative_path(runtime_path, module)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    if let Some(library) = strategy.c_abi_library.as_deref() {
        let path_like = library.contains('/')
            || library.contains('\\')
            || library.starts_with('.')
            || Path::new(library).is_absolute();
        if path_like {
            strategy.c_abi_library = Some(
                resolve_runtime_relative_path(runtime_path, library)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
}

pub(crate) fn verify_strategy_artifact(strategy: &StrategyRuntimeConfig) -> Result<(), String> {
    let Some(expected) = strategy.strategy_artifact_sha256.as_deref() else {
        return Ok(());
    };
    if let Some(reference) = strategy.external_executable.as_deref() {
        return qx_strategy::verify_file_sha256(reference, expected)
            .map_err(|error| format!("策略发布物校验失败 {}: {error}", reference));
    }
    let reference = strategy
        .python_module
        .as_deref()
        .ok_or_else(|| "strategy_artifact_sha256 缺少策略文件引用".to_string())?;
    // Python 可以配置 importable module name；Rust host 无法在不复制 Python
    // import 规则的情况下定位它，交由 Python worker 在 import 后按 __file__ 校验。
    // 显式文件路径仍在 spawn 前由 host 先校验，形成双重门禁。
    if !Path::new(reference).is_file()
        && !(reference.ends_with(".py")
            || reference.contains('/')
            || reference.contains('\\')
            || reference.starts_with('.')
            || Path::new(reference).is_absolute())
    {
        return Ok(());
    }
    qx_strategy::verify_file_sha256(reference, expected)
        .map_err(|error| format!("策略发布物校验失败 {}: {error}", reference))
}

#[cfg(test)]
pub(crate) fn strategy_current_qty(root: &Path, config: &RuntimeConfig) -> Result<i128, String> {
    let Some(instrument_text) = config.strategy.instrument.as_deref() else {
        return Ok(0);
    };
    let instrument = InstrumentId::parse(instrument_text)
        .ok_or_else(|| format!("Strategy instrument 非法: {instrument_text}"))?;
    strategy_current_qty_for(root, config, &instrument)
}

pub(crate) fn strategy_current_qty_for(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
) -> Result<i128, String> {
    let Some(account_id) = config.strategy.account_id.as_deref() else {
        return Ok(0);
    };
    // 多交易所套利的每条 intent 可以属于不同 venue；优先按
    // InstrumentId 的 venue 读取，不能把所有腿都误读成策略主腿。
    // 兼容旧的 paper/测试账户：历史配置可能把事件写入 strategy.venue_id
    // 对应的 EventLog，而 instrument 本身仍使用交易所 venue。
    let venue_id = instrument.venue.to_string();
    let Some(instrument_log_name) = account_event_log_name(account_id, &venue_id) else {
        return Err(format!(
            "Strategy 当前持仓暂不支持 venue_id={}；请先接入该 Venue 的账户事件归约",
            venue_id
        ));
    };
    let log_name = if event_log_exists(config, root, &instrument_log_name)? {
        instrument_log_name
    } else if let Some(configured_venue) = config.strategy.venue_id.as_deref() {
        let Some(configured_log_name) = account_event_log_name(account_id, configured_venue) else {
            return Ok(0);
        };
        if event_log_exists(config, root, &configured_log_name)? {
            configured_log_name
        } else {
            return Ok(0);
        }
    } else {
        return Ok(0);
    };
    let pipeline = open_runtime_pipeline(config, root, log_name, "USDT")
        .map_err(|error| format!("恢复 Strategy 账户 EventLog 失败: {error}"))?;
    Ok(pipeline
        .ledger()
        .position_for(account_id, instrument)
        .quantity
        .raw())
}

pub(crate) fn build_strategy_contract_input(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    request_id: &str,
    now: u64,
) -> Result<StrategyContractInput, String> {
    let account_id = config
        .strategy
        .account_id
        .clone()
        .ok_or_else(|| "Python Strategy 必须配置 account_id".to_string())?;
    let venue_id = config
        .strategy
        .venue_id
        .clone()
        .ok_or_else(|| "Python Strategy 必须配置 venue_id".to_string())?;
    if let Some(configured) = config.strategy.research_snapshot_path.as_deref() {
        let candidate = runtime_path(root, configured);
        let path = if candidate.exists() {
            candidate
        } else {
            PathBuf::from(configured)
        };
        let research = StrategyResearchSnapshot::from_json(
            &std::fs::read_to_string(&path).map_err(|error| {
                format!(
                    "读取 Python Strategy research snapshot 失败 {}: {error}",
                    path.display()
                )
            })?,
        )
        .map_err(|error| format!("Python Strategy research snapshot JSON 无效: {error:?}"))?;
        validate_research_snapshot_binding(&config.strategy, &research)?;
        let research_as_of = research.as_of;
        let data_fingerprint = research.candidate.config.data_fingerprint.clone();
        let (positions, cash, available_margin_raw, risk_state) =
            strategy_account_context(root, config, instrument)?;
        let context = StrategyContext {
            strategy_id: config
                .strategy
                .id
                .clone()
                .unwrap_or_else(|| config.strategy.version.clone()),
            strategy_version: config.strategy.version.clone(),
            data_fingerprint,
            as_of: research.as_of,
            research,
            account_id,
            venue_id,
            positions,
            cash,
            available_margin_raw,
            risk_state,
        };
        context
            .validate(now, config.environment.eq_ignore_ascii_case("production"))
            .map_err(|error| format!("Python StrategyContext 校验失败: {error}"))?;
        let bars = load_strategy_contract_bars(root, config, instrument, research_as_of)?
            .map(|(bars, _, _)| bars);
        return context.to_contract_input(request_id, instrument, bars);
    }

    let (positions, cash, available_margin_raw, risk_state) =
        strategy_account_context(root, config, instrument)?;
    let bars = load_strategy_contract_bars(root, config, instrument, now)?;
    let (bars, data_fingerprint, as_of) = if let Some((bars, data_fingerprint, as_of)) = bars {
        (Some(bars), data_fingerprint, as_of)
    } else {
        (None, "runtime-config-v1".into(), now.max(1))
    };
    let input = StrategyContractInput {
        schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: request_id.to_string(),
        strategy_id: config
            .strategy
            .id
            .clone()
            .unwrap_or_else(|| config.strategy.version.clone()),
        strategy_version: config.strategy.version.clone(),
        data_fingerprint,
        as_of,
        instrument: instrument.to_string(),
        positions,
        cash,
        available_margin_raw,
        risk_state,
        research_targets: BTreeMap::from([(instrument.to_string(), config.strategy.target_qty)]),
        bars,
    };
    input.validate()?;
    Ok(input)
}

pub(crate) const PYTHON_STRATEGY_TIMEOUT_MS: u64 = 2_000;

enum StrategyWireResponse {
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
        let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| "python".into());
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
        )
    }

    pub(crate) fn start_process_with_transport_config(
        executable: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        timeout_ms: u64,
        label: &str,
        transport: StrategyTransport,
        ring_config: SharedRingConfig,
    ) -> Result<Self, String> {
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
            .map_err(|error| format!("启动 {label} worker 失败: {error}"))?;
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
                            let _ = self.child.kill();
                            return Err(format!(
                                "{} worker 响应超时 timeout_ms={}{}",
                                self.label,
                                self.timeout_ms,
                                self.diagnostics()
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
                self.responses
                    .as_ref()
                    .ok_or_else(|| format!("{} worker 响应通道不可用", self.label))?
                    .recv_timeout(Duration::from_millis(self.timeout_ms))
                    .map_err(|error| match error {
                        mpsc::RecvTimeoutError::Timeout => {
                            let _ = self.child.kill();
                            format!(
                                "{} worker 响应超时 timeout_ms={}{}",
                                self.label,
                                self.timeout_ms,
                                self.diagnostics()
                            )
                        }
                        mpsc::RecvTimeoutError::Disconnected => {
                            format!("{} worker 响应通道已断开{}", self.label, self.diagnostics())
                        }
                    })??
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

fn python_module_search_path() -> Result<std::ffi::OsString, String> {
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

fn decode_python_strategy_response(
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

fn strategy_contract_output_from_native_decision(
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
    if let Some(window) = strategy.builtin_fast_window {
        config.fast_window = window;
    }
    if let Some(window) = strategy.builtin_slow_window {
        config.slow_window = window;
    }
    if let Some(period) = strategy.builtin_period {
        config.period = period;
    }
    if let Some(threshold) = strategy.builtin_threshold_bps {
        config.threshold_bps = threshold;
    }
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

enum ContractStrategyClient {
    Process(Box<PythonStrategyClient>),
    Native(Box<DynamicCAbiStrategy>),
}

/// 把持久化 Python/C++ JSONL 策略和受信任 C ABI 策略接入与 Rust
/// 原生策略相同的 Bar 回测循环。
/// 回测引擎传入的 history 已经按 `as_of` 截止，因此跨语言策略不会看到当前
/// 正在撮合的 Bar；输出仍必须经过统一的 OrderIntent、Risk 和 OMS 转换。
pub(crate) struct ContractBarStrategy {
    config: RuntimeConfig,
    client: ContractStrategyClient,
    instrument: InstrumentId,
    initial_cash: Money,
    currency: String,
    data_fingerprint: String,
    native_initialized: bool,
}

impl ContractBarStrategy {
    pub(crate) fn from_config(
        config: RuntimeConfig,
        frame: &BarFrame,
        initial_cash: Money,
        currency: impl Into<String>,
        dataset_bundle_fingerprint: Option<&str>,
    ) -> Result<Self, String> {
        let instrument = config
            .strategy
            .instrument
            .as_deref()
            .and_then(InstrumentId::parse)
            .ok_or_else(|| "跨语言回测策略缺少合法 strategy.instrument".to_string())?;
        if instrument != frame.instrument {
            return Err(format!(
                "跨语言回测 instrument 不一致: strategy={} frame={}",
                instrument, frame.instrument
            ));
        }
        let client = if let Some(module) = config.strategy.python_module.as_deref() {
            ContractStrategyClient::Process(Box::new(
                PythonStrategyClient::start_with_transport_config(
                    module,
                    config.strategy.python_timeout_ms,
                    config.strategy.transport,
                    SharedRingConfig {
                        capacity: config.strategy.shared_memory_capacity,
                        slot_bytes: config.strategy.shared_memory_slot_bytes,
                    },
                    config.strategy.strategy_artifact_sha256.as_deref(),
                )?,
            ))
        } else if let Some(executable) = config.strategy.external_executable.as_deref() {
            ContractStrategyClient::Process(Box::new(
                StrategyProcessClient::start_process_with_transport_config(
                    executable,
                    &config.strategy.external_args,
                    &config.strategy.external_env,
                    config.strategy.python_timeout_ms,
                    "外部 Strategy",
                    config.strategy.transport,
                    SharedRingConfig {
                        capacity: config.strategy.shared_memory_capacity,
                        slot_bytes: config.strategy.shared_memory_slot_bytes,
                    },
                )?,
            ))
        } else if let Some(library) = config.strategy.c_abi_library.as_deref() {
            let _ = library;
            let native = load_c_abi_strategy(&config.strategy)?;
            ContractStrategyClient::Native(Box::new(native))
        } else {
            return Err(
                "跨语言回测必须配置 strategy.python_module、external_executable 或 c_abi_library"
                    .into(),
            );
        };
        Ok(Self {
            config,
            client,
            instrument,
            initial_cash,
            currency: currency.into(),
            data_fingerprint: dataset_bundle_fingerprint
                .map(|fingerprint| format!("dataset-bundle:{fingerprint}"))
                .unwrap_or_else(|| format!("{:016x}", frame.digest())),
            native_initialized: false,
        })
    }
}

impl BarStrategy for ContractBarStrategy {
    fn on_bar_orders_checked(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        let Some(visible) = history.last() else {
            return Ok(Vec::new());
        };
        if instrument != &self.instrument || visible.ts >= ts {
            return Err(qx_core::QxError::Invariant(
                "跨语言 Bar 策略收到不可见或未绑定的 Bar".into(),
            ));
        }
        let bars = StrategyContractBars {
            source: "backtest-bar-history-v1".into(),
            ts: history.iter().map(|bar| bar.ts).collect(),
            open_raw: history.iter().map(|bar| bar.open).collect(),
            high_raw: history.iter().map(|bar| bar.high).collect(),
            low_raw: history.iter().map(|bar| bar.low).collect(),
            close_raw: history.iter().map(|bar| bar.close).collect(),
            volume_raw: history.iter().map(|bar| bar.volume).collect(),
        };
        let strategy_id = self
            .config
            .strategy
            .id
            .clone()
            .unwrap_or_else(|| self.config.strategy.version.clone());
        let input = StrategyContractInput {
            schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
            request_id: format!(
                "{strategy_id}:backtest:{visible_ts}",
                visible_ts = visible.ts
            ),
            strategy_id: strategy_id.clone(),
            strategy_version: self.config.strategy.version.clone(),
            data_fingerprint: self.data_fingerprint.clone(),
            as_of: visible.ts,
            instrument: instrument.to_string(),
            positions: BTreeMap::from([(instrument.to_string(), position)]),
            cash: BTreeMap::from([(self.currency.clone(), self.initial_cash.raw())]),
            available_margin_raw: Some(self.initial_cash.raw()),
            risk_state: "backtest-verified".into(),
            research_targets: BTreeMap::new(),
            bars: Some(bars),
        };
        let output = match &mut self.client {
            ContractStrategyClient::Process(client) => client
                .request(&input)
                .map_err(qx_core::QxError::BusinessViolation)?,
            ContractStrategyClient::Native(strategy) => {
                let context = native_strategy_context(&self.config.strategy, &input);
                let event = NativeMarketEvent::Bar {
                    instrument: instrument.clone(),
                    ts: visible.ts,
                    open_raw: visible.open,
                    high_raw: visible.high,
                    low_raw: visible.low,
                    close_raw: visible.close,
                    volume_raw: visible.volume,
                };
                invoke_c_abi_strategy(
                    strategy,
                    &mut self.native_initialized,
                    &context,
                    &input,
                    &event,
                )
                .map_err(qx_core::QxError::BusinessViolation)?
            }
        };
        let mut orders = Vec::with_capacity(output.intents.len().max(1));
        if output.intents.is_empty() {
            let rebalance = output
                .build_rebalance_plan(&input, 10_000, 1)
                .map_err(qx_core::QxError::BusinessViolation)?;
            if rebalance.positions.is_empty() {
                return Ok(Vec::new());
            }
            if let Some(order) = build_strategy_order_with_signal(
                &self.config,
                &strategy_id,
                output.signal_id,
                visible.ts,
                position,
                output.target_qty,
                Some(&output),
            )
            .map_err(qx_core::QxError::BusinessViolation)?
            {
                orders.push(order);
            }
        } else {
            for intent in &output.intents {
                orders.push(
                    build_strategy_order_from_contract_intent(
                        &self.config,
                        &strategy_id,
                        output.signal_id,
                        intent,
                        visible.ts,
                        position,
                    )
                    .map_err(qx_core::QxError::BusinessViolation)?,
                );
            }
        }
        Ok(orders)
    }
}

pub(crate) fn strategy_target_qty(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    now: u64,
) -> Result<i128, String> {
    if let Some(configured) = config.strategy.research_snapshot_path.as_deref() {
        let candidate = runtime_path(root, configured);
        let path = if candidate.exists() {
            candidate
        } else {
            PathBuf::from(configured)
        };
        let research = StrategyResearchSnapshot::from_json(
            &std::fs::read_to_string(&path).map_err(|error| {
                format!(
                    "读取 Strategy research snapshot 失败 {}: {error}",
                    path.display()
                )
            })?,
        )
        .map_err(|error| format!("Strategy research snapshot JSON 无效: {error:?}"))?;
        validate_research_snapshot_binding(&config.strategy, &research)?;
        let account_id = config
            .strategy
            .account_id
            .clone()
            .ok_or_else(|| "research snapshot 策略必须配置 account_id".to_string())?;
        let venue_id = config
            .strategy
            .venue_id
            .clone()
            .ok_or_else(|| "research snapshot 策略必须配置 venue_id".to_string())?;
        let data_fingerprint = research.candidate.config.data_fingerprint.clone();
        let (positions, cash, available_margin_raw, risk_state) =
            strategy_account_context(root, config, instrument)?;
        let context = StrategyContext {
            strategy_id: config
                .strategy
                .id
                .clone()
                .unwrap_or_else(|| config.strategy.version.clone()),
            strategy_version: config.strategy.version.clone(),
            data_fingerprint,
            as_of: research.as_of,
            research,
            account_id,
            venue_id,
            positions,
            cash,
            available_margin_raw,
            risk_state,
        };
        context
            .validate(now, config.environment.eq_ignore_ascii_case("production"))
            .map_err(|error| format!("StrategyContext 校验失败: {error}"))?;
        return context.target_for(instrument).ok_or_else(|| {
            format!(
                "Strategy research snapshot 未提供 instrument={} 的目标仓位",
                instrument
            )
        });
    }
    let Some(configured) = config.strategy.target_snapshot_path.as_deref() else {
        return Ok(config.strategy.target_qty);
    };
    let candidate = runtime_path(root, configured);
    let path = if candidate.exists() {
        candidate
    } else {
        PathBuf::from(configured)
    };
    let snapshot: StrategyTargetSnapshot =
        serde_json::from_str(&std::fs::read_to_string(&path).map_err(|error| {
            format!(
                "读取 Strategy target snapshot 失败 {}: {error}",
                path.display()
            )
        })?)
        .map_err(|error| format!("Strategy target snapshot JSON 无效: {error}"))?;
    snapshot.validate_for(&config.strategy.version, now)?;
    snapshot.target_for(instrument).ok_or_else(|| {
        format!(
            "Strategy target snapshot 未提供 instrument={} 的目标仓位",
            instrument
        )
    })
}

type StrategyAccountContext = (
    BTreeMap<String, i128>,
    BTreeMap<String, i128>,
    Option<i128>,
    String,
);

fn load_strategy_contract_bars(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
    as_of: u64,
) -> Result<Option<(StrategyContractBars, String, u64)>, String> {
    let Some(configured) = config.strategy.bars_snapshot_path.as_deref() else {
        return Ok(None);
    };
    let candidate = runtime_path(root, configured);
    let path = if candidate.exists() {
        candidate
    } else {
        PathBuf::from(configured)
    };
    let payload = std::fs::read_to_string(&path).map_err(|error| {
        format!(
            "读取 Strategy bars_snapshot_path 失败 {}: {error}",
            path.display()
        )
    })?;
    let frame = BarFrame::from_json(&payload).map_err(|error| {
        format!(
            "Strategy bars_snapshot_path BarFrame 无效 {}: {error:?}",
            path.display()
        )
    })?;
    if &frame.instrument != instrument {
        return Err(format!(
            "Strategy bars_snapshot_path instrument 不一致: strategy={} frame={}",
            instrument, frame.instrument
        ));
    }
    let visible: Vec<usize> = frame
        .ts
        .iter()
        .enumerate()
        .filter_map(|(index, ts)| (*ts <= as_of).then_some(index))
        .collect();
    let Some(last_index) = visible.last().copied() else {
        return Err(format!(
            "Strategy bars_snapshot_path 在 as_of={} 前没有可见 Bar",
            as_of
        ));
    };
    let bars = StrategyContractBars {
        source: frame.source.0.clone(),
        ts: visible.iter().map(|index| frame.ts[*index]).collect(),
        open_raw: visible.iter().map(|index| frame.open_raw[*index]).collect(),
        high_raw: visible.iter().map(|index| frame.high_raw[*index]).collect(),
        low_raw: visible.iter().map(|index| frame.low_raw[*index]).collect(),
        close_raw: visible
            .iter()
            .map(|index| frame.close_raw[*index])
            .collect(),
        volume_raw: visible
            .iter()
            .map(|index| frame.volume_raw[*index])
            .collect(),
    };
    bars.validate()?;
    Ok(Some((
        bars,
        format!("barframe:{:016x}", frame.digest()),
        frame.ts[last_index],
    )))
}

fn strategy_account_context(
    root: &Path,
    config: &RuntimeConfig,
    instrument: &InstrumentId,
) -> Result<StrategyAccountContext, String> {
    let account_id = config
        .strategy
        .account_id
        .as_deref()
        .ok_or_else(|| "StrategyContext 缺少 account_id".to_string())?;
    let venue_id = config
        .strategy
        .venue_id
        .as_deref()
        .ok_or_else(|| "StrategyContext 缺少 venue_id".to_string())?;
    let Some(log_name) = account_event_log_name(account_id, venue_id) else {
        return Err(format!(
            "StrategyContext 当前不支持 venue_id={} 的账户事实归约",
            venue_id
        ));
    };
    if !event_log_exists(config, root, &log_name)? {
        return Ok((
            BTreeMap::new(),
            BTreeMap::new(),
            None,
            "account-snapshot-not-seen".into(),
        ));
    }
    let pipeline = open_runtime_pipeline(config, root, log_name, "USDT")
        .map_err(|error| format!("恢复 StrategyContext 账户 EventLog 失败: {error}"))?;
    let state = pipeline.ledger().position_for(account_id, instrument);
    let positions = [(instrument.to_string(), state.quantity.raw())]
        .into_iter()
        .collect();
    let cash = pipeline.ledger().cash_balances_for(account_id);
    let available_margin_raw = pipeline
        .ledger()
        .equity_for(account_id, pipeline.marks(), "USDT");
    Ok((
        positions,
        cash,
        available_margin_raw,
        "ledger-replayed-account-state".into(),
    ))
}

#[cfg(test)]
pub(crate) fn build_strategy_order(
    config: &RuntimeConfig,
    strategy_id: &str,
    run_id: u64,
    now: u64,
    current_qty: i128,
    target_qty: i128,
) -> Result<Option<Order>, String> {
    build_strategy_order_with_signal(
        config,
        strategy_id,
        run_id,
        now,
        current_qty,
        target_qty,
        None,
    )
}

/// 将跨语言 Strategy API v1 的单笔 intent 转为统一核心订单。
/// 该转换只负责语义翻译，订单仍必须经过 RiskExecutionContext、OMS 和 Venue。
pub(crate) fn build_strategy_order_from_contract_intent(
    config: &RuntimeConfig,
    strategy_id: &str,
    signal_id: u64,
    intent: &StrategyContractIntent,
    now: u64,
    current_qty: i128,
) -> Result<Order, String> {
    let account_id = config
        .strategy
        .account_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Strategy 产生订单必须配置 account_id".to_string())?;
    let instrument = InstrumentId::parse(&intent.instrument)
        .ok_or_else(|| format!("Strategy intent instrument 非法: {}", intent.instrument))?;
    let side = match intent.side.to_ascii_lowercase().as_str() {
        "buy" => Side::Buy,
        "sell" => Side::Sell,
        other => return Err(format!("Strategy intent side 非法: {other}")),
    };
    let product = config.strategy.product.unwrap_or(TradingProduct::Spot);
    let position_mode = match intent.position_mode.as_deref() {
        None => config
            .strategy
            .position_mode
            .unwrap_or(PositionMode::OneWay),
        Some("one_way") => PositionMode::OneWay,
        Some("hedge") => PositionMode::Hedge,
        Some(other) => return Err(format!("Strategy intent position_mode 非法: {other}")),
    };
    let margin_mode = match intent.margin_mode.as_deref() {
        None => config
            .strategy
            .margin_mode
            .unwrap_or(if product == TradingProduct::Spot {
                MarginMode::Cash
            } else {
                MarginMode::Cross
            }),
        Some("cash") => MarginMode::Cash,
        Some("cross") => MarginMode::Cross,
        Some("isolated") => MarginMode::Isolated,
        Some(other) => return Err(format!("Strategy intent margin_mode 非法: {other}")),
    };
    let leverage = intent
        .leverage
        .unwrap_or(config.strategy.leverage.unwrap_or(1));
    let allow_short = if margin_mode == MarginMode::Cash {
        false
    } else {
        config
            .strategy
            .allow_short
            .unwrap_or(product.is_derivative())
    };
    let position_side = match intent
        .position_side
        .as_deref()
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("long") => PositionSide::Long,
        Some("short") => PositionSide::Short,
        Some("net") | None if position_mode == PositionMode::OneWay => PositionSide::Net,
        Some("net") | None => {
            if side == Side::Buy {
                PositionSide::Long
            } else {
                PositionSide::Short
            }
        }
        Some(other) => return Err(format!("Strategy intent position_side 非法: {other}")),
    };
    let policy = OrderPolicy {
        reduce_only: intent.reduce_only,
        position_side,
        margin_mode,
        position_mode,
        leverage,
        post_only: intent.post_only,
    };
    let mut order = Order {
        client_id: intent.intent_id,
        instrument,
        side,
        qty: qx_core::Quantity::from_raw(intent.qty_raw),
        limit: intent.limit_price_raw.map(Price::from_raw),
        status: OrderStatus::PendingSubmit,
        filled: qx_core::Quantity::ZERO,
        account_id: account_id.into(),
        trace: Some(qx_core::OrderTrace {
            strategy_id: Some(strategy_id.into()),
            signal_id: Some(signal_id),
            intent_id: Some(intent.intent_id),
            rule_version: Some(config.strategy.version.clone()),
        }),
        policy: None,
    };
    if product != TradingProduct::Spot
        || config.strategy.product.is_some()
        || config.strategy.margin_mode.is_some()
        || config.strategy.position_mode.is_some()
        || config.strategy.leverage.is_some()
        || intent.margin_mode.is_some()
        || intent.position_mode.is_some()
        || intent.leverage.is_some()
        || intent.reduce_only
        || intent.post_only
        || intent.position_side.is_some()
    {
        order.policy = Some(policy);
    }
    order
        .validate()
        .map_err(|error| format!("Strategy API v1 OrderIntent 转订单失败: {error}"))?;
    let mut risk = RiskGate::new();
    risk.add(Box::new(MaxQtyRule {
        max_qty: 1_000 * SCALE,
    }));
    if !allow_short {
        risk.add(Box::new(NoShortRule));
    }
    risk.check(&order, &PositionSnapshot::new(current_qty, 0))
        .map_err(|error| format!("Strategy API v1 RiskGate 拒绝 OrderIntent: {error:?}"))?;
    let _ = now;
    Ok(order)
}

pub(crate) fn build_strategy_order_with_signal(
    config: &RuntimeConfig,
    strategy_id: &str,
    run_id: u64,
    now: u64,
    current_qty: i128,
    target_qty: i128,
    contract_output: Option<&StrategyContractOutput>,
) -> Result<Option<Order>, String> {
    let instrument_text = config
        .strategy
        .instrument
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Strategy 产生订单必须配置 instrument".to_string())?;
    let current = qx_portfolio::PortfolioState {
        portfolio_id: strategy_id.into(),
        timestamp: now,
        cash: 0,
        positions: BTreeMap::from([(instrument_text.to_string(), current_qty)]),
    };
    let rebalance = qx_portfolio::rebalance(
        &current,
        &[qx_portfolio::TargetPosition {
            instrument: instrument_text.to_string(),
            quantity: target_qty,
        }],
        &qx_portfolio::PortfolioConstraint {
            max_turnover_bps: 10_000,
            min_trade_size: 1,
        },
    )?;
    if rebalance.positions.is_empty() {
        return Ok(None);
    }
    let account_id = config
        .strategy
        .account_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Strategy 产生订单必须配置 account_id".to_string())?;
    let instrument = InstrumentId::parse(instrument_text)
        .ok_or_else(|| format!("Strategy instrument 非法: {instrument_text}"))?;
    let signal = Signal {
        strategy_id: strategy_id.into(),
        signal_id: contract_output
            .map(|output| output.signal_id)
            .unwrap_or_else(|| run_id.max(1)),
        instrument: instrument.clone(),
        target_qty,
        confidence: contract_output
            .map(|output| output.confidence)
            .unwrap_or(1_000),
        priority: contract_output.map(|output| output.priority).unwrap_or(0),
        expires_at: contract_output
            .map(|output| {
                if output.expires_at == 0 {
                    now
                } else {
                    output.expires_at
                }
            })
            .unwrap_or(now),
    };
    let targets = SignalMerger.merge(vec![signal], now);
    let target = targets
        .first()
        .ok_or_else(|| "Strategy Signal 已过期或为空".to_string())?;
    let Some(intent) = rebalance_intent(
        target,
        current_qty,
        strategy_id,
        account_id,
        run_id.max(1),
        now,
    ) else {
        return Ok(None);
    };
    let order = intent.into_order();
    let product = config.strategy.product.unwrap_or(TradingProduct::Spot);
    let allow_short = config
        .strategy
        .allow_short
        .unwrap_or(product.is_derivative());
    let position_mode = config
        .strategy
        .position_mode
        .unwrap_or(PositionMode::OneWay);
    let margin_mode = config
        .strategy
        .margin_mode
        .unwrap_or(if product == TradingProduct::Spot {
            MarginMode::Cash
        } else {
            MarginMode::Cross
        });
    let leverage = config.strategy.leverage.unwrap_or(1);
    let mut order = order;
    if product != TradingProduct::Spot
        || config.strategy.product.is_some()
        || config.strategy.margin_mode.is_some()
        || config.strategy.position_mode.is_some()
        || config.strategy.leverage.is_some()
    {
        order.policy = Some(OrderPolicy {
            reduce_only: false,
            position_side: if position_mode == PositionMode::OneWay {
                PositionSide::Net
            } else if target_qty >= 0 {
                PositionSide::Long
            } else {
                PositionSide::Short
            },
            margin_mode,
            position_mode,
            leverage,
            post_only: false,
        });
    }
    order
        .validate()
        .map_err(|error| format!("Strategy OrderIntent 转订单失败: {error}"))?;
    let mut risk = RiskGate::new();
    risk.add(Box::new(MaxQtyRule {
        max_qty: 1_000 * SCALE,
    }));
    if !allow_short {
        risk.add(Box::new(NoShortRule));
    }
    risk.check(&order, &PositionSnapshot::new(current_qty, 0))
        .map_err(|error| format!("Strategy RiskGate 拒绝 OrderIntent: {error:?}"))?;
    Ok(Some(order))
}

pub(crate) fn strategy_submit_command(
    strategy_id: &str,
    order: &Order,
    dry_run: bool,
    spread_group_id: Option<&str>,
) -> Result<ControlCommand, String> {
    let mut payload = BTreeMap::from([(
        "order_json".into(),
        serde_json::to_string(order)
            .map_err(|error| format!("Strategy 订单序列化失败: {error}"))?,
    )]);
    if let Some(group_id) = spread_group_id {
        if group_id.trim().is_empty() {
            return Err("Strategy 多腿 spread_group_id 不能为空".into());
        }
        payload.insert("spread_group_id".into(), group_id.into());
    }
    Ok(ControlCommand {
        command_id: order.client_id,
        request_id: format!("strategy:{strategy_id}:{}", order.client_id),
        operator_id: strategy_id.into(),
        reason: "strategy signal -> portfolio -> risk -> order intent".into(),
        kind: CommandKind::SubmitOrder,
        target: order.client_id.to_string(),
        payload,
        permission: Permission::Trading,
        dry_run,
    })
}

pub(crate) fn persist_strategy_submit(
    control_store: &ControlStateBackend,
    command_queue: &dyn ControlCommandQueueBackend,
    command: &ControlCommand,
    now: u64,
) -> Result<String, String> {
    let (plane, result) = control_store
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, now))
        .map_err(|error| format!("Strategy SubmitOrder Accepted 持久化失败: {error}"))?;
    match result {
        Ok(_) => {
            command_queue
                .enqueue_command(command.clone(), now)
                .map_err(|error| format!("Strategy SubmitOrder 入队失败: {error:?}"))?;
            Ok("ORDER_INTENT_ACCEPTED".into())
        }
        Err(
            qx_control::ControlError::DuplicateCommand(_)
            | qx_control::ControlError::DuplicateRequest(_),
        ) => {
            let existing = plane
                .command(command.command_id)
                .ok_or_else(|| "Strategy 幂等命令缺少原命令".to_string())?;
            if existing.digest() != command.digest() {
                return Err("Strategy command_id 已被不同 OrderIntent 占用".into());
            }
            let status = plane
                .audit()
                .iter()
                .rev()
                .find(|record| record.command_id == command.command_id)
                .map(|record| record.status)
                .ok_or_else(|| "Strategy 幂等命令缺少审计记录".to_string())?;
            match status {
                qx_control::CommandStatus::Accepted => {
                    command_queue
                        .enqueue_command(command.clone(), now)
                        .map_err(|error| {
                            format!("Strategy 幂等 SubmitOrder 入队失败: {error:?}")
                        })?;
                    Ok("ORDER_INTENT_ALREADY_ACCEPTED".into())
                }
                qx_control::CommandStatus::Executed => Ok("ORDER_INTENT_ALREADY_EXECUTED".into()),
                qx_control::CommandStatus::Failed => {
                    Err("Strategy 原 OrderIntent 已执行失败".into())
                }
                qx_control::CommandStatus::Rejected => Err("Strategy 原 OrderIntent 已拒绝".into()),
            }
        }
        Err(error) => Err(format!("Strategy SubmitOrder 被控制面拒绝: {error:?}")),
    }
}

pub(crate) fn strategy_command(argv: &[String]) {
    let action = argv.get(2).cloned().unwrap_or_else(|| "list".into());
    match action.as_str() {
        "list" => {
            for kind in BuiltinStrategyKind::ALL {
                println!("{}\t{}", kind.name(), kind.description());
            }
        }
        "init" => {
            let name = match argv.get(3).cloned() {
                Some(value) => value,
                None => {
                    eprintln!("strategy init 需要 strategy 名称");
                    std::process::exit(2);
                }
            };
            let positional = argv
                .iter()
                .skip(4)
                .filter(|&argument| !argument.starts_with('-'))
                .cloned()
                .collect::<Vec<_>>();
            let output = positional
                .first()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(format!("qianxing.strategy.{name}.json")));
            let bars = positional.get(1).map(PathBuf::from);
            let force = argv.iter().any(|argument| argument == "--force");
            if let Err(error) = run_strategy_init(&name, &output, bars.as_deref(), force) {
                eprintln!("策略初始化失败: {error}");
                std::process::exit(2);
            }
        }
        "backtest" => {
            let runtime = match argv.get(3).cloned() {
                Some(value) => PathBuf::from(value),
                None => {
                    eprintln!("strategy backtest 需要 runtime.json bar-frame.json");
                    std::process::exit(2);
                }
            };
            let bars = match argv.get(4).cloned() {
                Some(value) => PathBuf::from(value),
                None => {
                    eprintln!("strategy backtest 缺少 bar-frame.json");
                    std::process::exit(2);
                }
            };
            let spec = argv.get(5).cloned().map(PathBuf::from);
            if let Err(error) = run_strategy_backtest(&runtime, &bars, spec.as_deref()) {
                eprintln!("策略回测失败: {error}");
                std::process::exit(2);
            }
        }
        _ => {
            eprintln!("strategy 仅支持 list、init、backtest");
            std::process::exit(2);
        }
    }
}

pub(crate) fn builtin_strategies_command(_argv: &[String]) {
    for kind in BuiltinStrategyKind::ALL {
        println!("{}\t{}", kind.name(), kind.description());
    }
}
