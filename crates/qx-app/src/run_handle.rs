//! Cooperative lifecycle handle for long application runs.

use crate::{AppError, AppErrorCategory};
use serde::{Deserialize, Serialize};
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Cancelling,
    Succeeded,
    Cancelled,
    Failed,
}

/// Lifecycle event emitted by the Rust worker that owns a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunEventKind {
    Started,
    CancellationRequested,
    Succeeded,
    Cancelled,
    Failed,
}

/// Sequenced local run event. Cursors are exclusive: `events_after(n)` returns
/// events whose sequence is strictly greater than `n`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvent {
    pub seq: u64,
    pub run_id: String,
    pub kind: RunEventKind,
    pub status: RunStatus,
}

fn push_event(events: &mut Vec<RunEvent>, run_id: &str, kind: RunEventKind, status: RunStatus) {
    events.push(RunEvent {
        seq: events.len() as u64 + 1,
        run_id: run_id.to_string(),
        kind,
        status,
    });
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
    events: Arc<Mutex<Vec<RunEvent>>>,
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
        let events = Arc::new(Mutex::new(Vec::new()));
        push_event(
            &mut events.lock().unwrap_or_else(|error| error.into_inner()),
            &run_id,
            RunEventKind::Started,
            RunStatus::Running,
        );
        let worker_events = Arc::clone(&events);
        let worker_run_id = run_id.clone();
        let (sender, receiver) = mpsc::channel();
        let worker_sender = sender.clone();
        let spawn_result = thread::Builder::new().spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                work(worker_cancellation.clone())
            }))
            .unwrap_or_else(|payload| {
                let message = payload
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("non-string panic payload");
                Err(AppError::new(
                    AppErrorCategory::InternalInvariant,
                    format!("RunHandle worker panicked: {message}"),
                ))
            });
            let terminal = match &result {
                Ok(Some(_)) => SUCCEEDED,
                Ok(None) => CANCELLED,
                Err(_) => FAILED,
            };
            let status = status_from_code(terminal);
            let kind = event_kind_for_status(status);
            let mut events = worker_events
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            // State and events share this lock with status()/events_after().
            // Publish both before releasing it so pollers cannot observe a
            // terminal state/event without its matching counterpart.
            let _ = worker_sender.send(result);
            worker_state.store(terminal, Ordering::Release);
            push_event(&mut events, &worker_run_id, kind, status);
        });
        if let Err(error) = spawn_result {
            state.store(FAILED, Ordering::Release);
            push_event(
                &mut events.lock().unwrap_or_else(|poison| poison.into_inner()),
                &run_id,
                RunEventKind::Failed,
                RunStatus::Failed,
            );
            let _ = sender.send(Err(AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("启动回测 worker 失败: {error}"),
            )));
        }
        Self {
            run_id,
            cancellation,
            state,
            events,
            receiver: Mutex::new(receiver),
            result: None,
        }
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn cancel(&self) {
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.state.load(Ordering::Acquire) == RUNNING {
            self.cancellation.cancel();
            self.state.store(CANCELLING, Ordering::Release);
            push_event(
                &mut events,
                &self.run_id,
                RunEventKind::CancellationRequested,
                RunStatus::Cancelling,
            );
        }
    }

    pub fn events_after(&self, cursor: u64) -> Vec<RunEvent> {
        self.events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter(|event| event.seq > cursor)
            .cloned()
            .collect()
    }

    pub fn events_json_after(&self, cursor: u64) -> Result<String, AppError> {
        serde_json::to_string(&self.events_after(cursor)).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("RunEvent 序列化失败: {error}"),
            )
        })
    }

    pub fn status(&mut self) -> RunStatus {
        // Worker terminal publication and cancellation both use the event
        // lock. Taking it before polling the result prevents observing the
        // state half of a terminal transition before its event is appended.
        let _events = self
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner());
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
        status_from_code(self.state.load(Ordering::Acquire))
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

fn status_from_code(code: u8) -> RunStatus {
    match code {
        CANCELLING => RunStatus::Cancelling,
        SUCCEEDED => RunStatus::Succeeded,
        CANCELLED => RunStatus::Cancelled,
        FAILED => RunStatus::Failed,
        _ => RunStatus::Running,
    }
}

fn event_kind_for_status(status: RunStatus) -> RunEventKind {
    match status {
        RunStatus::Succeeded => RunEventKind::Succeeded,
        RunStatus::Cancelled => RunEventKind::Cancelled,
        RunStatus::Failed => RunEventKind::Failed,
        RunStatus::Running | RunStatus::Cancelling => {
            unreachable!("worker terminal state must be terminal")
        }
    }
}

impl<T> Drop for RunHandle<T> {
    fn drop(&mut self) {
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.state.load(Ordering::Acquire) == RUNNING {
            self.cancellation.cancel();
            self.state.store(CANCELLING, Ordering::Release);
            push_event(
                &mut events,
                &self.run_id,
                RunEventKind::CancellationRequested,
                RunStatus::Cancelling,
            );
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
        let events = handle.events_after(0);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, RunEventKind::Started);
        assert_eq!(events[1].kind, RunEventKind::Succeeded);
        assert_eq!(events[1].seq, 2);
        assert_eq!(handle.events_after(1), vec![events[1].clone()]);
        assert!(handle.events_after(2).is_empty());
    }

    #[test]
    fn terminal_success_always_has_its_result_ready_for_pollers() {
        for ordinal in 0..128 {
            let mut handle =
                RunHandle::spawn(format!("run-poll-{ordinal}"), move |_| Ok(Some(ordinal)));
            loop {
                let status = handle.status();
                let events = handle.events_after(0);
                let terminal_event = events
                    .last()
                    .is_some_and(|event| event.kind == RunEventKind::Succeeded);
                if status == RunStatus::Succeeded || terminal_event {
                    assert_eq!(handle.status(), RunStatus::Succeeded);
                    assert_eq!(
                        handle.events_after(0).last().unwrap().kind,
                        RunEventKind::Succeeded
                    );
                    break;
                }
                thread::yield_now();
            }
            assert_eq!(handle.try_take_result().unwrap().unwrap(), ordinal);
        }
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
        let events = handle.events_after(0);
        assert_eq!(
            events.iter().map(|event| event.kind).collect::<Vec<_>>(),
            vec![
                RunEventKind::Started,
                RunEventKind::CancellationRequested,
                RunEventKind::Cancelled
            ]
        );
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
    fn worker_panics_become_terminal_errors_and_emit_failed_event() {
        let mut handle = RunHandle::<u32>::spawn("run-panic", |_| -> Result<_, AppError> {
            panic!("panic payload preserved")
        });
        assert_eq!(handle.wait(), RunStatus::Failed);
        let error = handle.try_take_result().unwrap().unwrap_err();
        assert!(error.message().contains("panic payload preserved"));
        assert_eq!(
            handle.events_after(0).last().unwrap().kind,
            RunEventKind::Failed
        );
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
