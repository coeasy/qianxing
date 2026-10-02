//! 停机请求的生产者与带预算的 worker 汇合阶梯。
//!
//! `ShutdownToken` 只描述"已经请求停机"，它需要一个真的会响的生产者；这里把进程收到
//! 的终止请求（Ctrl+C / SIGINT，以及 Unix 下的 SIGTERM）落成一次
//! [`RuntimeSupervisor::request_shutdown`]，并按 `shutdown_timeout_ms` 给汇合设上限。

use super::*;

static SHUTDOWN_SIGNALLED: AtomicBool = AtomicBool::new(false);
static HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);

const POLL_INTERVAL_MS: u64 = 50;

extern "C" fn record_shutdown(_signal: libc::c_int) {
    SHUTDOWN_SIGNALLED.store(true, Ordering::Release);
}

/// 把进程级终止请求接到停机令牌上；幂等，可在每个 worker 入口调用。
/// 返回 `true` 表示本次真的安装了处理函数。
pub fn install_shutdown_signals() -> bool {
    if HANDLER_INSTALLED.swap(true, Ordering::AcqRel) {
        return false;
    }
    let handler = record_shutdown as *const () as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
    true
}

/// 进程是否收到过终止请求。
pub fn shutdown_signalled() -> bool {
    SHUTDOWN_SIGNALLED.load(Ordering::Acquire)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerLadder {
    /// worker 自己结束了，停机令牌从未被按下。
    Finished,
    /// 停机请求被转发，worker 在预算内退出；`waited_ms` 是**收到请求之后**等的时长。
    StoppedWithinBudget { waited_ms: u64 },
    /// 停机请求被按下但 worker 到预算仍未退出；调用方只能报告而不能强杀线程。
    /// `waited_ms` 同样从请求落下起算，不是从开始等待起算。
    StopTimedOut { waited_ms: u64 },
}

/// 等 worker 结束，期间把终止请求转发给 [`RuntimeSupervisor`]。
///
/// `worker_finished`/`signalled`/`now_ms`/`sleep_ms` 全部注入，因此停机阶梯不需要真信号、
/// 也不需要真等待就能被用例驱动。
pub fn wait_for_worker_finish(
    supervisor: &RuntimeSupervisor,
    worker_finished: impl Fn() -> bool,
    signalled: impl Fn() -> bool,
    now_ms: impl Fn() -> u64,
    mut sleep_ms: impl FnMut(u64),
) -> WorkerLadder {
    let budget = supervisor.config().shutdown_timeout_ms;
    let start = now_ms();
    let mut requested = supervisor.is_shutdown_requested();
    // 预算与 `waited_ms` 都从**观察到停机请求**起算。worker 可能在请求到来之前已经被等了一整天，
    // 从 `start` 起算会让它在请求落下的第一次轮询就判 `StopTimedOut`，一次宽限时间都不给。
    let mut requested_at = if requested { Some(start) } else { None };
    loop {
        if worker_finished() {
            let base = requested_at.unwrap_or(start);
            let waited_ms = now_ms().saturating_sub(base);
            return if requested {
                WorkerLadder::StoppedWithinBudget { waited_ms }
            } else {
                WorkerLadder::Finished
            };
        }
        if !requested && signalled() {
            supervisor.request_shutdown();
            requested = true;
            requested_at = Some(now_ms());
        }
        if let Some(at) = requested_at {
            let waited_ms = now_ms().saturating_sub(at);
            if waited_ms > budget {
                return WorkerLadder::StopTimedOut { waited_ms };
            }
        }
        sleep_ms(POLL_INTERVAL_MS);
    }
}
