//! 托管子进程的收尾回收：先 kill，再在预算内轮询；到期仍在世的要点名报出去。

use std::process::{Child, ChildStdin};
use std::thread;
use std::time::{Duration, Instant};

pub(crate) struct ManagedChild {
    pub(crate) id: String,
    pub(crate) child: Child,
    pub(crate) stop_channel: Option<ChildStdin>,
}

/// 一轮回收的判定结果。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReapDecision {
    Done,
    KeepWaiting { sleep: Duration },
    TimedOut,
}

/// 轮询步长。收尾预算可以小到 1 ms，步长必须塞得进剩余预算，否则"有界"只是名义上的。
const REAP_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// 纯判定：`alive[i]` 是第 i 个托管进程在上一轮 `try_wait` 之后是否仍在世。
/// 先判"全收了没有"再判预算，否则最后一轮已经没有人活着的那种正常收尾会被说成超时。
pub(crate) fn reap_round(alive: &[bool], elapsed: Duration, budget: Duration) -> ReapDecision {
    if !alive.iter().any(|still_running| *still_running) {
        return ReapDecision::Done;
    }
    if elapsed >= budget {
        return ReapDecision::TimedOut;
    }
    ReapDecision::KeepWaiting {
        sleep: budget.saturating_sub(elapsed).min(REAP_POLL_INTERVAL),
    }
}

/// 预算用尽时要点名：漏掉任何一个 id，运维就不知道还有谁在写同一份 data_dir。
fn reap_timeout_report(stragglers: &[String], budget: Duration) -> String {
    format!(
        "{} 个托管 worker 已 kill 却没在 {} ms 收尾预算内退出: {}；父进程无法确认它们已停止，\
         这些进程可能仍在写同一份 data_dir，请手工确认",
        stragglers.len(),
        budget.as_millis(),
        stragglers.join(", ")
    )
}

/// kill 之后在 `budget` 内回收。`Child::wait()` 会一直堵到进程真的消失，父进程于是可能永远
/// 停在收尾这一步——而 `shutdown_timeout_ms` 正是给这段时间准备的预算（V11 L4）。预算用尽不是
/// "假装收工"：没收回来的 worker 会被点名成 Err，父进程不会带着孤儿进程静静退出。
pub(crate) fn stop_managed_children(
    children: &mut [ManagedChild],
    budget: Duration,
) -> Result<(), String> {
    for managed in children.iter_mut() {
        let _ = managed.child.kill();
    }
    let started = Instant::now();
    loop {
        let mut alive = Vec::with_capacity(children.len());
        for managed in children.iter_mut() {
            let status = managed
                .child
                .try_wait()
                .map_err(|error| format!("检查托管 worker {} 是否退出失败: {error}", managed.id))?;
            alive.push(status.is_none());
        }
        match reap_round(&alive, started.elapsed(), budget) {
            ReapDecision::Done => return Ok(()),
            ReapDecision::KeepWaiting { sleep } => thread::sleep(sleep),
            ReapDecision::TimedOut => {
                let stragglers = children
                    .iter()
                    .zip(alive.iter())
                    .filter(|(_, still_running)| **still_running)
                    .map(|(managed, _)| managed.id.clone())
                    .collect::<Vec<_>>();
                return Err(reap_timeout_report(&stragglers, budget));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn nothing_to_reap_is_done_without_consuming_the_budget() {
        assert_eq!(
            reap_round(&[], Duration::ZERO, ms(1_000)),
            ReapDecision::Done
        );
        // 空花名册即使预算已经是 0 也不报超时：没有进程要收，超时是无意义的话。
        assert_eq!(
            reap_round(&[], ms(5_000), Duration::ZERO),
            ReapDecision::Done
        );
    }

    #[test]
    fn a_fully_reaped_roster_wins_over_the_expired_budget() {
        // 反例形状：先判预算再判存活，最后一轮全部收工的正常收尾会被报成 TimedOut。
        assert_eq!(
            reap_round(&[false, false], ms(2_000), ms(1_000)),
            ReapDecision::Done
        );
    }

    #[test]
    fn the_budget_expires_exactly_on_the_dot() {
        assert_eq!(
            reap_round(&[false, true], ms(999), ms(1_000)),
            ReapDecision::KeepWaiting { sleep: ms(1) }
        );
        assert_eq!(
            reap_round(&[false, true], ms(1_000), ms(1_000)),
            ReapDecision::TimedOut
        );
        assert_eq!(
            reap_round(&[false, true], ms(1_001), ms(1_000)),
            ReapDecision::TimedOut
        );
    }

    #[test]
    fn waiting_never_sleeps_past_the_remaining_budget() {
        let budget = ms(1_000);
        let mut elapsed = Duration::ZERO;
        let mut rounds = 0_usize;
        loop {
            match reap_round(&[true], elapsed, budget) {
                ReapDecision::Done => panic!("有一个进程始终在世，不该判成收工"),
                ReapDecision::KeepWaiting { sleep } => {
                    assert!(!sleep.is_zero(), "睡着等于原地打转，这一轮必须真的推进时间");
                    assert!(sleep <= REAP_POLL_INTERVAL, "步长失控: {sleep:?}");
                    assert!(sleep <= budget.saturating_sub(elapsed), "这一觉会睡过预算");
                    elapsed += sleep;
                    rounds += 1;
                }
                ReapDecision::TimedOut => break,
            }
            assert!(rounds < 1_000, "轮询没有推进时间，收尾循环会永远转下去");
        }
        assert!(
            elapsed <= budget,
            "累计等待 {elapsed:?} 越过了预算 {budget:?}"
        );
        assert_eq!(rounds, 20, "1 s 预算按 50 ms 步长应该是 20 轮");
    }

    #[test]
    fn the_timeout_message_names_every_straggler() {
        // 预算用尽那条路在真进程上不确定（收得回就永远走不到），所以把"点名"这半步抽成
        // 纯函数来测：漏掉任何一个 id，运维就不知道还有谁在写同一份 data_dir。
        let report = reap_timeout_report(&["api".into(), "ccxt-market-main".into()], ms(10_000));
        assert!(report.contains("2 个托管 worker"), "{report}");
        assert!(report.contains("10000 ms"), "{report}");
        assert!(report.contains("api"), "{report}");
        assert!(report.contains("ccxt-market-main"), "{report}");
    }

    #[test]
    fn a_real_child_is_killed_and_reaped_inside_the_budget() {
        let mut children = vec![spawn_dummy_child("reap-real-child")];
        stop_managed_children(&mut children, ms(20_000)).expect("托管子进程应在预算内被回收");
        // 反向对照：回收之后 try_wait 仍报得出退出状态，不会退回 None 让人以为还在世。
        // 状态本身不判成败——夹具可能自己跑完退出，也可能被 kill 掉，两条路都算收干净。
        assert!(
            children[0]
                .child
                .try_wait()
                .expect("回收状态可查")
                .is_some(),
            "stop_managed_children 报 Ok 却还有没收到的子进程，说明父进程会带着孤儿退出"
        );
    }

    #[test]
    fn reaping_an_empty_roster_is_ok() {
        stop_managed_children(&mut [], ms(20_000)).expect("没有子进程时收尾不该报错");
    }

    /// 夹具子进程：把当前测试可执行文件当 worker 用。参数对 libtest 只是一条匹配不到的过滤
    /// 器，它打印 "running 0 tests" 后退出——确定性的托管对象，不碰真实 worker 语义。
    fn spawn_dummy_child(tag: &str) -> ManagedChild {
        let child = Command::new(std::env::current_exe().expect("测试可执行文件"))
            .arg(format!("qx_reap_dummy_{tag}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("夹具子进程可启动");
        ManagedChild {
            id: tag.into(),
            child,
            stop_channel: None,
        }
    }
}
