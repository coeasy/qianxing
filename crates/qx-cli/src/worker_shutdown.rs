//! CLI worker 的停机阶梯：把进程收到的终止请求接到 `ShutdownToken` 上。
//!
//! 每个 `run_*_worker` 此前都是 `spawn_worker(...)?.join()`：线程自己只按 `--once` 或
//! 错误退出，而 14 处 `context.should_stop()` 读的令牌没有任何生产者。这里补上生产者，
//! 并给汇合设 `shutdown_timeout_ms` 上限，让"优雅停机"这句话在命令面上真的成立。

use super::*;

/// 等 worker 线程结束；期间收到终止请求就转成停机请求，并等它按预算退出。
///
/// `role` 与 `worker_id` 分开传：调用点保持一行，报错文案仍带 worker 身份。
pub(crate) fn join_worker_handle(
    supervisor: &RuntimeSupervisor,
    handle: std::thread::JoinHandle<Result<(), String>>,
    role: &str,
    worker_id: &str,
) -> Result<(), String> {
    let label = format!("{role} worker {worker_id}");
    qx_runtime::install_shutdown_signals();
    let started = std::time::Instant::now();
    match qx_runtime::wait_for_worker_finish(
        supervisor,
        || handle.is_finished(),
        qx_runtime::shutdown_signalled,
        || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        |millis| thread::sleep(Duration::from_millis(millis)),
    ) {
        qx_runtime::WorkerLadder::Finished => {}
        qx_runtime::WorkerLadder::StoppedWithinBudget { waited_ms } => {
            println!("[停机 · Shutdown] {label} 按停机请求退出 waited_ms={waited_ms}")
        }
        qx_runtime::WorkerLadder::StopTimedOut { waited_ms } => {
            return Err(format!(
                "{label} 收到停机请求后 {waited_ms}ms 仍未退出，超过 shutdown_timeout_ms"
            ))
        }
    }
    handle.join().map_err(|_| format!("{label} panic"))?
}
