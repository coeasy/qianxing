//! `supervise` 的等待阶梯：子进程异常退出的上报，与终止请求下的优雅汇合。
//!
//! 托管循环此前只有一个出口——某个 worker 自己退出，因此 `stop_managed_children` 在
//! 正常停机时永远跑不到；这里把"收到终止请求"变成第二个出口，并给它设
//! `shutdown_timeout_ms` 预算：预算内等 worker 按自己的停机令牌收尾，超预算才判定失败。

const POLL_INTERVAL_MS: u64 = 250;

/// 一个子进程的退出结果：原始退出码与日志文字分开携带。
///
/// 只带文字的话上游没法按数值分支——Rust panic 的 101、OOM 的 137、干净退出的 0
/// 在字符串里长得一样，只能去读日志。被信号杀死时 `raw_code` 是 `None`。
pub(crate) struct ProcessExit {
    pub raw_code: Option<i32>,
    pub label: String,
}

/// 一个被托管的子进程；抽成 trait 是为了让等待阶梯能被用例驱动，不必真起进程。
pub(crate) trait ManagedProcess {
    fn id(&self) -> &str;

    /// 轮询一次退出状态；已退出时返回原始码与文字描述。
    fn poll_exit(&mut self) -> Result<Option<ProcessExit>, String>;

    /// 转发父进程停机请求。测试替身默认无副作用；真实子进程通过关闭 stdin pipe 收到 EOF。
    fn request_stop(&mut self) -> Result<(), String> {
        Ok(())
    }
}

pub(crate) enum SupervisorStop {
    /// 托管 worker 自己退出了：属于故障，调用方要停掉其余 worker 并按失败上报。
    /// `code` 是原始退出码（`None` = 被信号杀死），`label` 是日志用的文字描述。
    WorkerExited {
        id: String,
        code: Option<i32>,
        label: String,
    },
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
        let mut exited: Option<(String, Option<i32>, String)> = None;
        let mut running = 0_usize;
        for child in children.iter_mut() {
            match child.poll_exit()? {
                Some(exit) => {
                    if exited.is_none() {
                        exited = Some((child.id().to_string(), exit.raw_code, exit.label));
                    }
                }
                None => running += 1,
            }
        }
        if signalled() {
            if requested_at.is_none() {
                requested_at = Some(now_ms());
                for child in children.iter_mut() {
                    child.request_stop()?;
                }
            }
            let at = requested_at.expect("shutdown request timestamp was just set");
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
        } else if let Some((id, code, label)) = exited {
            return Ok(SupervisorStop::WorkerExited { id, code, label });
        }
        sleep_ms(POLL_INTERVAL_MS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::thread;
    use std::time::Duration;

    struct FakeChild {
        id: &'static str,
        exit_after_polls: usize,
        polls: Cell<usize>,
        stop_requests: Cell<usize>,
    }

    impl FakeChild {
        fn new(id: &'static str, exit_after_polls: usize) -> Self {
            Self {
                id,
                exit_after_polls,
                polls: Cell::new(0),
                stop_requests: Cell::new(0),
            }
        }
    }

    impl ManagedProcess for FakeChild {
        fn id(&self) -> &str {
            self.id
        }

        fn poll_exit(&mut self) -> Result<Option<ProcessExit>, String> {
            self.polls.set(self.polls.get() + 1);
            if self.polls.get() >= self.exit_after_polls {
                Ok(Some(ProcessExit {
                    raw_code: Some(0),
                    label: "0".into(),
                }))
            } else {
                Ok(None)
            }
        }

        fn request_stop(&mut self) -> Result<(), String> {
            self.stop_requests.set(self.stop_requests.get() + 1);
            Ok(())
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

    struct CodeChild {
        id: &'static str,
        code: i32,
    }

    impl ManagedProcess for CodeChild {
        fn id(&self) -> &str {
            self.id
        }

        fn poll_exit(&mut self) -> Result<Option<ProcessExit>, String> {
            Ok(Some(ProcessExit {
                raw_code: Some(self.code),
                label: self.code.to_string(),
            }))
        }
    }

    #[test]
    fn worker_exit_carries_the_raw_exit_code() {
        // `poll_exit` 只回文字时，panic 的 101、OOM 的 137 和干净退出的 0 在监控里长得
        // 一样，只能去读日志。这里钉住原始码从子进程一路传到 `WorkerExited`。
        let mut children = [CodeChild {
            id: "qx-strategy",
            code: 101,
        }];
        let stop =
            wait_for_children(&mut children, || false, || 0_u64, |_millis| {}, 10_000).unwrap();
        match stop {
            SupervisorStop::WorkerExited { id, code, label } => {
                assert_eq!(id, "qx-strategy");
                assert_eq!(code, Some(101));
                assert_eq!(label, "101");
            }
            other => panic!("expected WorkerExited, got {}", kind(&other)),
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
        assert_eq!(
            children[0].stop_requests.get(),
            1,
            "第一个 worker 没收到停机转发"
        );
        assert_eq!(
            children[1].stop_requests.get(),
            1,
            "第二个 worker 没收到停机转发"
        );
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
    fn child_runtime_stop_channel_exits_after_parent_closes_stdin() {
        if std::env::var_os("QX_TEST_STOP_CHANNEL_CHILD").is_none() {
            return;
        }
        assert!(qx_runtime::install_stdin_shutdown_listener());
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !qx_runtime::shutdown_signalled() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            qx_runtime::shutdown_signalled(),
            "stdin EOF 未到达运行时停机令牌"
        );
    }

    #[test]
    fn supervisor_forwards_stop_to_a_real_child_through_stdin_eof() {
        use std::process::{Command, Stdio};

        let executable = std::env::current_exe().expect("当前测试可执行文件");
        let mut child = match Command::new(executable)
            .arg("child_runtime_stop_channel_exits_after_parent_closes_stdin")
            .env("QX_TEST_STOP_CHANNEL_CHILD", "1")
            .env(
                qx_runtime::WORKER_STOP_CHANNEL_ENV,
                qx_runtime::WORKER_STOP_CHANNEL_STDIN_EOF,
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            // 本用例的全部价值都在那条 pipe 上，pipe 起不来就是整条没法跑：
            // Windows 受限沙箱会拒绝建 pipe（os error 231），而 Stdio::null() 照常可用。
            // 整条跳过而不是降级成假断言，子进程那一半由上一条用例单独钉住。
            Err(error) => {
                eprintln!("跳过停机 pipe 转发用例（父侧建 pipe 失败）：{error}");
                return;
            }
        };
        let stop_channel = child.stdin.take().expect("子进程 stdin pipe");
        let mut children = [super::super::reap::ManagedChild {
            id: "stop-channel-child".into(),
            child,
            stop_channel: Some(stop_channel),
        }];
        let started = std::time::Instant::now();
        let stop = wait_for_children(
            &mut children,
            || true,
            || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            |millis| thread::sleep(Duration::from_millis(millis.min(10))),
            2_000,
        )
        .expect("停机转发应成功");
        assert_eq!(kind(&stop), "stopped-within-budget");
        assert!(
            children[0]
                .child
                .try_wait()
                .expect("读取 worker 退出状态")
                .is_some(),
            "SupervisorStop::StoppedWithinBudget 不得在子进程仍活着时返回"
        );
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
