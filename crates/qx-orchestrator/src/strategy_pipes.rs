//! Bounded protocol and diagnostic pumps for external strategy worker pipes.

use qx_runtime::{read_capped_worker_line, record_stderr_tail, stderr_tail_note};
use qx_strategy::transport::StrategyTransport;
use qx_strategy::{StrategyFrame, DEFAULT_MAX_FRAME_BYTES};
use std::collections::VecDeque;
use std::io::BufReader;
use std::process::{ChildStderr, ChildStdout};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;

pub enum StrategyWorkerResponse {
    JsonLine(String),
    Frame(StrategyFrame),
}

pub struct StrategyWorkerPipes {
    pub responses: Option<Receiver<Result<StrategyWorkerResponse, String>>>,
    pub stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

/// Start bounded stdout/stderr readers. A capacity-one response channel applies backpressure
/// to the worker pipe; shared-memory transports intentionally have no stdout protocol pump.
pub fn pump_strategy_process(
    stdout: Option<ChildStdout>,
    stderr: ChildStderr,
    transport: StrategyTransport,
) -> StrategyWorkerPipes {
    let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
    let stderr_tail_reader = Arc::clone(&stderr_tail);
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        loop {
            let line = match read_capped_worker_line(&mut reader, DEFAULT_MAX_FRAME_BYTES) {
                Ok(Some(line)) => Ok(line),
                Ok(None) => return,
                Err(error) => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
            };
            let broken = line.is_err();
            if let Ok(mut tail) = stderr_tail_reader.lock() {
                if let Some(note) = stderr_tail_note(line) {
                    record_stderr_tail(&mut tail, note);
                }
            }
            if broken {
                return;
            }
        }
    });

    let responses = match transport {
        StrategyTransport::Jsonl | StrategyTransport::FramedJson => {
            let Some(stdout) = stdout else {
                return StrategyWorkerPipes {
                    responses: None,
                    stderr_tail,
                };
            };
            let (sender, responses) = mpsc::sync_channel(1);
            thread::spawn(move || match transport {
                StrategyTransport::Jsonl => {
                    let mut reader = BufReader::new(stdout);
                    loop {
                        let line =
                            match read_capped_worker_line(&mut reader, DEFAULT_MAX_FRAME_BYTES) {
                                Ok(Some(line)) => line,
                                Ok(None) => break,
                                Err(error) => {
                                    let _ = sender.send(Err(format!(
                                        "读取 Strategy worker 响应失败: {error}"
                                    )));
                                    return;
                                }
                            };
                        if sender
                            .send(Ok(StrategyWorkerResponse::JsonLine(line)))
                            .is_err()
                        {
                            return;
                        }
                    }
                    let _ = sender.send(Err("Strategy worker 已关闭输出".into()));
                }
                StrategyTransport::FramedJson => {
                    let mut reader = stdout;
                    loop {
                        match StrategyFrame::read_from(&mut reader, DEFAULT_MAX_FRAME_BYTES) {
                            Ok(frame) => {
                                if sender
                                    .send(Ok(StrategyWorkerResponse::Frame(frame)))
                                    .is_err()
                                {
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
        }
        StrategyTransport::SharedMemoryJson | StrategyTransport::SharedMemoryColumnar => None,
    };
    StrategyWorkerPipes {
        responses,
        stderr_tail,
    }
}
