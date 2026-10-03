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
    /// 收到终止请求后，全部 worker 在预算内退出；`waited_ms` 从请求落下起算。
    StoppedWithinBudget { waited_ms: u64 },
    /// 收到终止请求，但仍有 worker 到预算没退出；`waited_ms` 同样不含请求之前的等待。
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
    // 预算与 `waited_ms` 从**第一次观察到终止请求**起算：托管循环通常在子进程起来之前就开始等，
    // 从等待起点算的话，跑了一小时之后再按一次 Ctrl+C 会立刻判超时，宽限预算形同虚设。
    let mut requested_at: Option<u64> = None;
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
        if signalled() {
            let at = match requested_at {
                Some(at) => at,
                None => {
                    let at = now_ms();
                    requested_at = Some(at);
                    at
                }
            };
            let waited_ms = now_ms().saturating_sub(at);
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
            // 两个子进程恰好在"第一次观察到终止请求"那一轮就都退了：收到请求后的等待是 0ms。
            // 按进门计时这里会是 250（把请求之前的那一轮 sleep 也算进宽限）。
            SupervisorStop::StoppedWithinBudget { waited_ms } => assert_eq!(waited_ms, 0),
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
            // 请求落下的那一轮第一个子进程就退了，阶梯仍一直等到最后一个子进程（再三轮 = 750ms）。
            SupervisorStop::StoppedWithinBudget { waited_ms } => assert_eq!(waited_ms, 750),
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

    #[test]
    fn the_stop_budget_is_counted_from_the_request_not_from_the_start_of_waiting() {
        // 反向验证依据：预算按 `wait_for_children` 进门计时时，终止请求在第 5 次 sleep 之后
        // 落下（假时钟已走到 2_250，等了 1_250 > 预算 1_000），同一输入会当场判成 StopTimedOut。
        let harness = Harness::new(5);
        let mut children = [
            FakeChild::new("qx-scheduler", 7),
            FakeChild::new("qx-strategy", 7),
        ];
        let stop = outcomes(&mut children, &harness, 1_000).unwrap();
        assert_eq!(kind(&stop), "stopped-within-budget");
        match stop {
            SupervisorStop::StoppedWithinBudget { waited_ms } => {
                // 请求落下后再过两轮子进程才退完：报的必须是"收到请求之后等了 250ms"，
                // 不是"从开始等待算起 1_750ms"。
                assert_eq!(waited_ms, 250, "waited_ms 不再是收到停机请求后的时长");
            }
            other => unreachable!("长跑之后按停机请求退出不能判超时: {}", kind(&other)),
        }
    }
}
