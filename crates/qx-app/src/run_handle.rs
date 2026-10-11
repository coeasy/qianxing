//! Cooperative lifecycle handle for long application runs.

use crate::{AppError, AppErrorCategory};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const RUNNING: u8 = 0;
const CANCELLING: u8 = 1;
const SUCCEEDED: u8 = 2;
const CANCELLED: u8 = 3;
const FAILED: u8 = 4;

/// Finite default for [`RunHandle::wait`].
///
/// Plan §9: "`wait()` defaults to a finite wait; an unbounded wait has to be
/// explicitly requested by the caller and must be cancellable." A handle whose
/// default is "block forever" gives every caller an unbounded wait they never
/// asked for, so the default lives here and an unbounded wait has to be written
/// out as a `status()` loop. Same 30 s as the Python remote handle.
pub const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

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

    /// Block until the operation reaches a terminal state, or until
    /// [`DEFAULT_WAIT_TIMEOUT`] elapses. Hitting the deadline returns the
    /// **current** state and does not cancel the run; a caller who genuinely
    /// wants to wait without a bound writes that loop itself on `status()`.
    /// Cancellation stays cooperative inside the Rust engine, so another thread
    /// calling `cancel()` ends the wait.
    pub fn wait(&mut self) -> RunStatus {
        self.wait_within(DEFAULT_WAIT_TIMEOUT)
    }

    /// The bounded body of `wait`. Private on purpose: the public surface offers
    /// exactly two intents — the finite default above, and an explicitly
    /// unbounded `status()` loop the caller spells out.
    fn wait_within(&mut self, timeout: Duration) -> RunStatus {
        if self.result.is_none()
            && matches!(self.state.load(Ordering::Acquire), RUNNING | CANCELLING)
        {
            let received = self
                .receiver
                .lock()
                .map(|receiver| receiver.recv_timeout(timeout));
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
                // 到点没结果 = "还没好"，不是"坏了"：这一支必须与断连分开，否则一次
                // 正常的超时返回会把一个还在跑的运行记成 FAILED。
                Ok(Err(RecvTimeoutError::Timeout)) => {}
                Ok(Err(RecvTimeoutError::Disconnected)) | Err(_) => {
                    self.state.store(FAILED, Ordering::Release)
                }
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

    #[test]
    fn a_deadline_return_reports_the_current_state_instead_of_failing_the_run() {
        let mut handle = RunHandle::<u32>::spawn("run-bounded", |token| {
            while !token.is_cancelled() {
                thread::yield_now();
            }
            Ok(None)
        });
        // 到点 = 还没好，所以是 Running —— 把超时并进断连那一支会把它记成 Failed。
        assert_eq!(
            handle.wait_within(Duration::from_millis(20)),
            RunStatus::Running
        );
        // 缺省有限不代表不可取消：显式取消仍然收得回来。
        handle.cancel();
        assert_eq!(
            handle.wait_within(Duration::from_secs(5)),
            RunStatus::Cancelled
        );
    }

    #[test]
    fn the_public_default_wait_is_finite() {
        // 方案 §9「`wait()` 默认有限时」：缺省常量必须是有限且小的，不能悄悄变成"一直等"。
        assert!(DEFAULT_WAIT_TIMEOUT <= Duration::from_secs(60));
        let mut handle = RunHandle::<u32>::spawn("run-default", |_| Ok(Some(1_u32)));
        assert_eq!(handle.wait(), RunStatus::Succeeded);
    }
}
