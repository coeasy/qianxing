//! `supervise` 的等待阶梯：子进程异常退出的上报，与终止请求下的优雅汇合。
//!
//! 托管循环此前只有一个出口——某个 worker 自己退出，因此 `stop_managed_children` 在
//! 正常停机时永远跑不到；这里把"收到终止请求"变成第二个出口，并给它设
//! `shutdown_timeout_ms` 预算：预算内等 worker 按自己的停机令牌收尾，超预算才判定失败。

const POLL_INTERVAL_MS: u64 = 250;

/// 一个被托管的子进程；抽成 trait 是为了让等待阶梯能被用例驱动，不必真起进程。
pub(crate) trait ManagedProcess {
    fn id(&self) -> &str;

    /// 轮询一次退出状态；已退出时返回退出码描述。
    fn poll_exit(&mut self) -> Result<Option<String>, String>;
}

pub(crate) enum SupervisorStop {
    /// 托管 worker 自己退出了：属于故障，调用方要停掉其余 worker 并按失败上报。
    WorkerExited { id: String, code: String },
    /// 收到终止请求后，全部 worker 在预算内退出。
    StoppedWithinBudget { waited_ms: u64 },
    /// 收到终止请求，但仍有 worker 到预算没退出。
    StopTimedOut { waited_ms: u64, remaining: usize },
}

/// 轮询子进程直到出现结论：故障退出、按停机请求全部退出、或停机超预算。
///
/// `signalled`/`now_ms`/`sleep_ms` 全部注入，因此停机分支不需要真信号就能被用例驱动。
pub(crate) fn wait_for_children<P: ManagedProcess>(
    children: &mut [P],
    signalled: impl Fn() -> bool,
    now_ms: impl Fn() -> u64,
    mut sleep_ms: impl FnMut(u64),
    budget_ms: u64,
) -> Result<SupervisorStop, String> {
    let start = now_ms();
    loop {
        let mut exited: Option<(String, String)> = None;
        let mut running = 0_usize;
        for child in children.iter_mut() {
            match child.poll_exit()? {
                Some(code) => {
                    if exited.is_none() {
                        exited = Some((child.id().to_string(), code));
                    }
                }
                None => running += 1,
            }
        }
        let waited_ms = now_ms().saturating_sub(start);
        if signalled() {
            if running == 0 {
                return Ok(SupervisorStop::StoppedWithinBudget { waited_ms });
            }
            if waited_ms > budget_ms {
                return Ok(SupervisorStop::StopTimedOut {
                    waited_ms,
                    remaining: running,
                });
            }
        } else if let Some((id, code)) = exited {
            return Ok(SupervisorStop::WorkerExited { id, code });
        }
        sleep_ms(POLL_INTERVAL_MS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    struct FakeChild {
        id: &'static str,
        exit_after_polls: usize,
        polls: Cell<usize>,
    }

    impl FakeChild {
        fn new(id: &'static str, exit_after_polls: usize) -> Self {
            Self {
                id,
                exit_after_polls,
                polls: Cell::new(0),
            }
        }
    }

    impl ManagedProcess for FakeChild {
        fn id(&self) -> &str {
            self.id
        }

        fn poll_exit(&mut self) -> Result<Option<String>, String> {
            self.polls.set(self.polls.get() + 1);
            if self.polls.get() >= self.exit_after_polls {
                Ok(Some("0".into()))
            } else {
                Ok(None)
            }
        }
    }

    /// 驱动等待阶梯的假时钟：每次 sleep 推进墙钟，到点就落下终止请求。
    struct Harness {
        now_ms: Cell<u64>,
        sleeps: RefCell<Vec<u64>>,
        signal_after_sleeps: usize,
    }

    impl Harness {
        fn new(signal_after_sleeps: usize) -> Self {
            Self {
                now_ms: Cell::new(1_000),
                sleeps: RefCell::new(Vec::new()),
                signal_after_sleeps,
            }
        }

        fn signalled(&self) -> bool {
            self.sleeps.borrow().len() >= self.signal_after_sleeps
        }

        fn sleep_ms(&self, millis: u64) {
            self.sleeps.borrow_mut().push(millis);
            self.now_ms.set(self.now_ms.get() + millis);
        }
    }

    fn outcomes(
        children: &mut [FakeChild],
        harness: &Harness,
        budget_ms: u64,
    ) -> Result<SupervisorStop, String> {
        wait_for_children(
            children,
            || harness.signalled(),
            || harness.now_ms.get(),
            |millis| harness.sleep_ms(millis),
            budget_ms,
        )
    }

    fn kind(stop: &SupervisorStop) -> &'static str {
        match stop {
            SupervisorStop::WorkerExited { .. } => "worker-exited",
            SupervisorStop::StoppedWithinBudget { .. } => "stopped-within-budget",
            SupervisorStop::StopTimedOut { .. } => "stop-timed-out",
        }
    }

    #[test]
    fn worker_exit_without_shutdown_is_reported_as_failure() {
        let harness = Harness::new(usize::MAX);
        let mut children = [
            FakeChild::new("qx-scheduler", 99),
            FakeChild::new("qx-strategy", 2),
        ];
        let stop = outcomes(&mut children, &harness, 10_000).unwrap();
        assert!(matches!(
            stop,
            SupervisorStop::WorkerExited { ref id, .. } if id == "qx-strategy"
        ));
        assert_eq!(harness.sleeps.borrow().len(), 1);
    }

    #[test]
    fn shutdown_signal_turns_worker_exit_into_graceful_stop() {
        // 反向验证依据：删掉停机分支后，同一输入落回 WorkerExited（红）。
        let harness = Harness::new(1);
        let mut children = [
            FakeChild::new("qx-scheduler", 2),
            FakeChild::new("qx-strategy", 2),
        ];
        let stop = outcomes(&mut children, &harness, 10_000).unwrap();
        assert_eq!(kind(&stop), "stopped-within-budget");
        match stop {
            SupervisorStop::StoppedWithinBudget { waited_ms } => assert_eq!(waited_ms, 250),
            _ => unreachable!(),
        }
    }

    #[test]
    fn stop_waits_for_the_last_child_before_reporting_graceful_stop() {
        let harness = Harness::new(1);
        let mut children = [
            FakeChild::new("qx-scheduler", 2),
            FakeChild::new("qx-paper", 5),
        ];
        let stop = outcomes(&mut children, &harness, 10_000).unwrap();
        assert_eq!(kind(&stop), "stopped-within-budget");
        match stop {
            SupervisorStop::StoppedWithinBudget { waited_ms } => assert_eq!(waited_ms, 1_000),
            _ => unreachable!(),
        }
    }

    #[test]
    fn stuck_child_only_fails_after_the_shutdown_budget() {
        let harness = Harness::new(1);
        let mut children = [
            FakeChild::new("qx-scheduler", 2),
            FakeChild::new("qx-paper", usize::MAX),
        ];
        let stop = outcomes(&mut children, &harness, 1_000).unwrap();
        assert_eq!(kind(&stop), "stop-timed-out");
        match stop {
            SupervisorStop::StopTimedOut {
                waited_ms,
                remaining,
            } => {
                assert!(
                    waited_ms > 1_000,
                    "预算内不得提前判超时 waited_ms={waited_ms}"
                );
                assert_eq!(remaining, 1);
            }
            _ => unreachable!(),
        }
    }
}
