//! Cooperative lifecycle handle for long application runs.

use crate::{AppError, AppErrorCategory};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;

const RUNNING: u8 = 0;
const CANCELLING: u8 = 1;
const SUCCEEDED: u8 = 2;
const CANCELLED: u8 = 3;
const FAILED: u8 = 4;

/// Stable state view for SDKs polling a long-running application operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Cancelling,
    Succeeded,
    Cancelled,
    Failed,
}

/// A clonable cancellation signal passed into the Rust engine loop.
#[derive(Clone, Debug, Default)]
pub(crate) struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Owns one worker result and exposes cancellation/status without duplicating
/// engine behavior in an SDK wrapper.
pub struct RunHandle<T> {
    run_id: String,
    cancellation: CancellationToken,
    state: Arc<AtomicU8>,
    receiver: Mutex<Receiver<Result<Option<T>, AppError>>>,
    result: Option<Result<T, AppError>>,
}

impl<T: Send + 'static> RunHandle<T> {
    pub(crate) fn spawn<F>(run_id: impl Into<String>, work: F) -> Self
    where
        F: FnOnce(CancellationToken) -> Result<Option<T>, AppError> + Send + 'static,
    {
        let run_id = run_id.into();
        let cancellation = CancellationToken::default();
        let worker_cancellation = cancellation.clone();
        let state = Arc::new(AtomicU8::new(RUNNING));
        let worker_state = Arc::clone(&state);
        let (sender, receiver) = mpsc::channel();
        let worker_sender = sender.clone();
        let spawn_result = thread::Builder::new().spawn(move || {
            let result = work(worker_cancellation.clone());
            let terminal = match &result {
                Ok(Some(_)) => SUCCEEDED,
                Ok(None) => CANCELLED,
                Err(_) => FAILED,
            };
            let _ = worker_sender.send(result);
            worker_state.store(terminal, Ordering::Release);
        });
        if let Err(error) = spawn_result {
            state.store(FAILED, Ordering::Release);
            let _ = sender.send(Err(AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("启动回测 worker 失败: {error}"),
            )));
        }
        Self {
            run_id,
            cancellation,
            state,
            receiver: Mutex::new(receiver),
            result: None,
        }
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn cancel(&self) {
        if self.state.load(Ordering::Acquire) == RUNNING {
            self.cancellation.cancel();
            self.state.store(CANCELLING, Ordering::Release);
        }
    }

    pub fn status(&mut self) -> RunStatus {
        if self.result.is_none() {
            let received = self.receiver.lock().map(|receiver| receiver.try_recv());
            match received {
                Err(_) => self.state.store(FAILED, Ordering::Release),
                Ok(Ok(Ok(Some(value)))) => {
                    self.state.store(SUCCEEDED, Ordering::Release);
                    self.result = Some(Ok(value));
                }
                Ok(Ok(Ok(None))) => self.state.store(CANCELLED, Ordering::Release),
                Ok(Ok(Err(error))) => {
                    self.state.store(FAILED, Ordering::Release);
                    self.result = Some(Err(error));
                }
                Ok(Err(TryRecvError::Empty | TryRecvError::Disconnected)) => {}
            }
        }
        match self.state.load(Ordering::Acquire) {
            CANCELLING => RunStatus::Cancelling,
            SUCCEEDED => RunStatus::Succeeded,
            CANCELLED => RunStatus::Cancelled,
            FAILED => RunStatus::Failed,
            _ => RunStatus::Running,
        }
    }

    /// Block until the operation reaches a terminal state, then return that
    /// state. Cancellation remains cooperative inside the Rust engine.
    pub fn wait(&mut self) -> RunStatus {
        if self.result.is_none()
            && matches!(self.state.load(Ordering::Acquire), RUNNING | CANCELLING)
        {
            let received = self.receiver.lock().map(|receiver| receiver.recv());
            match received {
                Ok(Ok(Ok(Some(value)))) => {
                    self.state.store(SUCCEEDED, Ordering::Release);
                    self.result = Some(Ok(value));
                }
                Ok(Ok(Ok(None))) => self.state.store(CANCELLED, Ordering::Release),
                Ok(Ok(Err(error))) => {
                    self.state.store(FAILED, Ordering::Release);
                    self.result = Some(Err(error));
                }
                Ok(Err(_)) | Err(_) => self.state.store(FAILED, Ordering::Release),
            }
        }
        self.status()
    }

    /// Return a terminal result once available. `None` means still running or
    /// cancelled; callers distinguish those cases with `status()`.
    pub fn try_take_result(&mut self) -> Option<Result<T, AppError>> {
        let _ = self.status();
        self.result.take()
    }
}

impl<T> Drop for RunHandle<T> {
    fn drop(&mut self) {
        if matches!(self.state.load(Ordering::Acquire), RUNNING | CANCELLING) {
            self.cancellation.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_handle_reports_success_and_delivers_the_result_once() {
        let mut handle = RunHandle::spawn("run-success", |_| Ok(Some(17_u32)));
        assert_eq!(handle.wait(), RunStatus::Succeeded);
        assert_eq!(handle.try_take_result().unwrap().unwrap(), 17);
        assert!(handle.try_take_result().is_none());
    }

    #[test]
    fn cancellation_is_visible_and_waits_for_cooperative_worker_exit() {
        let mut handle = RunHandle::<u32>::spawn("run-cancel", |token| {
            while !token.is_cancelled() {
                thread::yield_now();
            }
            Ok(None)
        });
        handle.cancel();
        assert_eq!(handle.wait(), RunStatus::Cancelled);
        assert!(handle.try_take_result().is_none());
    }

    #[test]
    fn worker_errors_are_terminal_and_retrievable() {
        let mut handle = RunHandle::<u32>::spawn("run-error", |_| {
            Err(AppError::new(AppErrorCategory::InvalidInput, "bad spec"))
        });
        assert_eq!(handle.wait(), RunStatus::Failed);
        let error = handle.try_take_result().unwrap().unwrap_err();
        assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    }
}
