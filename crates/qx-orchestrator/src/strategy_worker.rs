//! Request/response lifecycle for external strategy workers.

use crate::{
    pump_strategy_process, spawn_strategy_process, stop_strategy_process, StrategySharedRings,
    StrategyWorkerResponse,
};
use qx_process_io::write_all_within;
use qx_strategy::transport::StrategyTransport;
use qx_strategy::{
    SharedRingConfig, SharedRingError, StrategyFrame, StrategyFrameKind, DEFAULT_MAX_FRAME_BYTES,
};
use std::collections::VecDeque;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub struct StrategyWorkerProcess {
    child: Option<std::process::Child>,
    stdin: Option<std::process::ChildStdin>,
    responses: Option<mpsc::Receiver<Result<StrategyWorkerResponse, String>>>,
    shared_rings: Option<StrategySharedRings>,
    timeout_ms: u64,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    label: String,
    diagnostic_program: String,
    transport: StrategyTransport,
    next_sequence: u64,
}

pub struct StrategyWorkerSpawnConfig<'a> {
    pub executable: &'a str,
    pub args: &'a [String],
    pub env: &'a std::collections::BTreeMap<String, String>,
    pub timeout_ms: u64,
    pub label: &'a str,
    pub diagnostic_program: &'a str,
    pub transport: StrategyTransport,
    pub ring_config: SharedRingConfig,
    pub ring_directory: Option<&'a std::path::Path>,
}

impl StrategyWorkerProcess {
    pub fn spawn(config: StrategyWorkerSpawnConfig<'_>) -> Result<Self, String> {
        let StrategyWorkerSpawnConfig {
            executable,
            args,
            env,
            timeout_ms,
            label,
            diagnostic_program,
            transport,
            ring_config,
            ring_directory,
        } = config;
        let mut actual_args = args.to_vec();
        let shared_rings = if matches!(
            transport,
            StrategyTransport::SharedMemoryJson | StrategyTransport::SharedMemoryColumnar
        ) {
            let rings = ring_directory
                .map_or_else(
                    || StrategySharedRings::create(ring_config),
                    |directory| StrategySharedRings::create_in(ring_config, directory),
                )
                .map_err(|error| format!("创建 {label} 共享 rings 失败: {error}"))?;
            actual_args.extend(shared_ring_arguments(
                transport,
                rings.input_path(),
                rings.output_path(),
                ring_config,
                rings.process_id(),
            ));
            Some(rings)
        } else {
            None
        };
        let pipes =
            spawn_strategy_process(executable, &actual_args, env, transport).map_err(|error| {
                format!("启动 {label} worker 失败（程序={diagnostic_program}）: {error}")
            })?;
        let pumped = pump_strategy_process(pipes.stdout, pipes.stderr, transport);
        Ok(Self {
            child: Some(pipes.child),
            stdin: pipes.stdin,
            responses: pumped.responses,
            shared_rings,
            timeout_ms,
            stderr_tail: pumped.stderr_tail,
            label: label.to_string(),
            diagnostic_program: diagnostic_program.to_string(),
            transport,
            next_sequence: 1,
        })
    }

    pub fn diagnostics(&self) -> String {
        let Ok(tail) = self.stderr_tail.lock() else {
            return "; stderr=（stderr 泵锁中毒，尾部不可读）".to_string();
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

    fn death_note(&mut self) -> String {
        let exit = match self.child.as_mut() {
            None => "worker 子进程句柄不可用".to_string(),
            Some(child) => match child.try_wait() {
                Ok(Some(status)) => match status.code() {
                    Some(code) => format!("退出码 {code}"),
                    None => "无退出码（信号终止）".to_string(),
                },
                Ok(None) => "进程未退出".to_string(),
                Err(error) => format!("退出码不可读: {error}"),
            },
        };
        let tail = self.diagnostics();
        if tail.is_empty() {
            format!(
                "（程序={}，{exit}，worker 无 stderr 输出；若该程序是 WindowsApps 的 python 占位桩，请把 QX_PYTHON 指向可用解释器）",
                self.diagnostic_program
            )
        } else {
            format!("（程序={}，{exit}{tail}）", self.diagnostic_program)
        }
    }

    fn kill_child(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
    }

    /// Send one logical request and wait for its matching transport response under one timeout.
    pub fn request(
        &mut self,
        json_payload: String,
        columnar_payload: Option<Vec<u8>>,
    ) -> Result<String, String> {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        let response = match self.transport {
            StrategyTransport::SharedMemoryJson | StrategyTransport::SharedMemoryColumnar => {
                let payload = if self.transport == StrategyTransport::SharedMemoryColumnar {
                    columnar_payload
                        .ok_or_else(|| format!("{} worker 缺少列式共享内存请求", self.label))?
                } else {
                    json_payload.into_bytes()
                };
                let frame = StrategyFrame::request(sequence, payload)
                    .encode(DEFAULT_MAX_FRAME_BYTES)
                    .map_err(|error| format!("编码 {} 共享分帧输入失败: {error}", self.label))?;
                let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
                let rings = self
                    .shared_rings
                    .as_mut()
                    .ok_or_else(|| format!("{} 共享输入 ring 不可用", self.label))?;
                loop {
                    let writer = rings
                        .input_mut()
                        .ok_or_else(|| format!("{} 共享输入 ring 不可用", self.label))?;
                    match writer.try_push(&frame) {
                        Ok(()) => break,
                        Err(SharedRingError::Full) if Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(1))
                        }
                        Err(SharedRingError::Full) => {
                            let note = self.death_note();
                            self.kill_child();
                            return Err(format!(
                                "{} worker 输入写入超时 timeout_ms={}：共享输入 ring 满，worker 已停止消费{}",
                                self.label, self.timeout_ms, note
                            ));
                        }
                        Err(error) => {
                            return Err(format!("写入 {} 共享 ring 失败: {error}", self.label));
                        }
                    }
                }
                let rings = self
                    .shared_rings
                    .as_mut()
                    .ok_or_else(|| format!("{} 共享输出 ring 不可用", self.label))?;
                loop {
                    let reader = rings
                        .output_mut()
                        .ok_or_else(|| format!("{} 共享输出 ring 不可用", self.label))?;
                    match reader.try_pop_frame() {
                        Ok(frame) => break StrategyWorkerResponse::Frame(frame),
                        Err(SharedRingError::Empty) if Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(1))
                        }
                        Err(SharedRingError::Empty) => {
                            let note = self.death_note();
                            self.kill_child();
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
                let bytes = if self.transport == StrategyTransport::Jsonl {
                    format!("{json_payload}\n").into_bytes()
                } else {
                    StrategyFrame::request(sequence, json_payload.into_bytes())
                        .encode(DEFAULT_MAX_FRAME_BYTES)
                        .map_err(|error| format!("编码 {} 分帧输入失败: {error}", self.label))?
                };
                let stdin = self
                    .stdin
                    .take()
                    .ok_or_else(|| format!("{} worker stdin 不可用", self.label))?;
                let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
                let written = write_all_within(
                    stdin,
                    bytes,
                    deadline.saturating_duration_since(Instant::now()),
                    || self.kill_child(),
                );
                match written {
                    Ok(stdin) => self.stdin = Some(stdin),
                    Err(error) => {
                        let note = self.death_note();
                        return Err(format!("写入 {} 输入失败: {error}{note}", self.label));
                    }
                }
                let received = self
                    .responses
                    .as_ref()
                    .ok_or_else(|| format!("{} worker 响应通道不可用", self.label))?
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()));
                match received {
                    Ok(Ok(response)) => response,
                    Ok(Err(error)) => return Err(format!("{error}{}", self.death_note())),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let note = self.death_note();
                        self.kill_child();
                        return Err(format!(
                            "{} worker 响应超时 timeout_ms={}{}",
                            self.label, self.timeout_ms, note
                        ));
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        let note = self.death_note();
                        return Err(format!("{} worker 响应通道已断开{note}", self.label));
                    }
                }
            }
        };
        response_payload(response, sequence, &self.label)
    }
}

fn response_payload(
    response: StrategyWorkerResponse,
    expected_sequence: u64,
    label: &str,
) -> Result<String, String> {
    match response {
        StrategyWorkerResponse::JsonLine(line) => Ok(line),
        StrategyWorkerResponse::Frame(frame) => {
            if frame.sequence != expected_sequence {
                return Err(format!(
                    "{} worker 响应序号不匹配: expected={} actual={}",
                    label, expected_sequence, frame.sequence
                ));
            }
            if !matches!(
                frame.kind,
                StrategyFrameKind::Response | StrategyFrameKind::Error
            ) {
                return Err(format!("{} worker 返回非响应分帧: {:?}", label, frame.kind));
            }
            String::from_utf8(frame.payload)
                .map_err(|error| format!("{} worker 响应不是 UTF-8: {error}", label))
        }
    }
}

#[cfg(test)]
mod tests;
impl Drop for StrategyWorkerProcess {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            let budget = Duration::from_millis(self.timeout_ms.clamp(1, 1_000));
            if let Err(error) = stop_strategy_process(
                child,
                format!("strategy:{}", self.label),
                self.stdin.take(),
                budget,
            ) {
                eprintln!("[Strategy worker] 有界回收未完成: {error}");
            }
        }
        self.shared_rings.take();
    }
}

pub fn shared_ring_arguments(
    transport: StrategyTransport,
    input_path: &std::path::Path,
    output_path: &std::path::Path,
    ring_config: SharedRingConfig,
    parent_pid: u32,
) -> Vec<String> {
    let protocol = if transport == StrategyTransport::SharedMemoryColumnar {
        "shared_memory_columnar"
    } else {
        "shared_memory_json"
    };
    vec![
        "--protocol".into(),
        protocol.into(),
        "--input-ring".into(),
        input_path.to_string_lossy().into_owned(),
        "--output-ring".into(),
        output_path.to_string_lossy().into_owned(),
        "--ring-capacity".into(),
        ring_config.capacity.to_string(),
        "--ring-slot-bytes".into(),
        ring_config.slot_bytes.to_string(),
        "--parent-pid".into(),
        parent_pid.to_string(),
    ]
}
