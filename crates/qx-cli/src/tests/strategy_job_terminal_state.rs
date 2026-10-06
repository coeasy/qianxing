//! 策略作业运行的终态收口（V13 R2 第十二遍 #190）。
//!
//! 两条互补的断言，都落在真实 worker 入口上：
//! 1. 作业体抛错时，这条 `JobRun` 必须当场写成 `Failed` 并带上固定错误码 —— 否则
//!    「策略自己报错了」只剩「被下一轮调度 tick 升级成 TIMEOUT」一条出口，读起来像跑太久。
//! 2. 队列条目可能在回写终态之后、`ack` 之前掉电：条目还在、运行却已收口。worker 领取租约后
//!    必须先认 Scheduler 的终态并跳过，否则同一笔作业会被执行两次（第二次就是二次提交）。

use super::*;

struct StrategyJobCase {
    dir: PathBuf,
    data: PathBuf,
    config_path: PathBuf,
    store: JsonStateStore,
    state_path: PathBuf,
    queue: FileJobQueue,
}

/// 一个只含本次用例作业的运行时：作业清单走 `load_scheduler_state` 的正常装载闸门。
/// `broken_target` 把 `strategy.target_snapshot_path` 指到一个不存在的快照，让作业体在
/// 「算目标仓位」这一步就确定性报错 —— instrument 本身没法写非法值，配置序列化会先拒。
fn strategy_job_case(label: &str, broken_target: bool) -> StrategyJobCase {
    let dir = temp_cli_case_dir(label);
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data.to_string_lossy().into_owned();
    if broken_target {
        config.strategy.target_snapshot_path = Some("no-such-target-snapshot.json".into());
    }
    let jobs_path = dir.join("jobs.json");
    std::fs::write(
        &jobs_path,
        serde_json::to_string(&[case_job_spec()]).expect("序列化 JobSpec 失败"),
    )
    .unwrap();
    config.scheduler.jobs_path = jobs_path.to_string_lossy().into_owned();
    let config_path = dir.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();
    let (store, _, state_path) = load_scheduler_state(&config, &data, &config_path).unwrap();
    StrategyJobCase {
        dir,
        queue: FileJobQueue::new(data.join("job-queue")),
        data,
        config_path,
        store,
        state_path,
    }
}

fn case_job_spec() -> JobSpec {
    JobSpec {
        job_id: "strategy-terminal-case".into(),
        job_version: "1".into(),
        owner: "strategy-paper".into(),
        enabled: true,
        trigger: Trigger::Cron("* * * * *".into()),
        window: JobWindow::Any,
        depends_on: Vec::new(),
        input_refs: vec!["dataset:terminal-case".into()],
        output_refs: vec!["terminal-case-report".into()],
        timeout_seconds: 60,
        retry_policy: RetryPolicy::default(),
        concurrency_key: "strategy-terminal-case".into(),
        idempotency_key: "strategy-terminal-case:idem".into(),
        permission_scope: "report".into(),
        audit_reason: "terminal-state-case".into(),
        dry_run: false,
    }
}

/// 建一条 `Running` 运行并原样入队；返回 `run_id`。
///
/// 队列信封只收 `Running` 的运行（`QueuedJob::validate`），所以「条目还在、运行已收口」
/// 的重投形态只能是：条目带的是入队那一刻的 `Running` 副本，Scheduler 状态里同一条运行
/// 已经写成终态。用例因此先把运行入队，再改动状态侧的结局。
fn seed_run_and_enqueue(case: &StrategyJobCase) -> Result<u64, String> {
    let job = case_job_spec();
    let run = case
        .store
        .transact_scheduler_at(&case.state_path, |scheduler| {
            scheduler
                .start_run_at("strategy-terminal-case", "20260927", 7, 1_700_000_000)
                .map_err(|error| format!("创建 JobRun 失败: {error:?}"))
        })
        .map_err(|error| format!("Scheduler 状态事务失败: {error:?}"))?
        .1?;
    case.queue
        .enqueue(job, run.clone(), 1_700_000_000)
        .map_err(|error| format!("写入 JobQueue 失败: {error:?}"))?;
    Ok(run.run_id)
}

/// 只改 Scheduler 状态里那条运行的结局，队列条目保持入队时的 `Running` 副本。
fn finalize_run(case: &StrategyJobCase, run_id: u64, success: bool, code: &str) {
    case.store
        .transact_scheduler_at(&case.state_path, |scheduler| {
            scheduler
                .finish_run_with_code(run_id, success, Some(code), 1_700_000_060)
                .map(|_| ())
                .map_err(|error| format!("收口 JobRun 失败: {error:?}"))
        })
        .map_err(|error| format!("Scheduler 状态事务失败: {error:?}"))
        .expect("收口夹具运行失败")
        .1
        .expect("收口夹具运行被拒绝");
}

fn run_case(case: &StrategyJobCase) -> Result<(), String> {
    run_strategy_worker(&case.config_path, "strategy-paper", true)
}

/// 作业体报错必须当场把运行写成 `Failed` + 固定错误码，且默认重试策略下不伪造 `next_retry_ts`
/// （生产里没有任何 `retry_run` 调用者，写了就是空头承诺）。
#[test]
fn strategy_job_failure_finalizes_its_run_instead_of_waiting_for_a_timeout() {
    let case = strategy_job_case("v13r2p12-failed-writeback", true);
    let run_id = seed_run_and_enqueue(&case).unwrap();
    let error = run_case(&case).expect_err("目标快照读不回时作业体必须确定性报错");
    assert!(
        error.contains("读取 Strategy target snapshot 失败"),
        "报错被终态回写改写了身份: {error}"
    );
    let run = case
        .store
        .load_scheduler_at(&case.state_path)
        .unwrap()
        .run(run_id)
        .cloned()
        .expect("作业运行应在 Scheduler 状态里");
    assert_eq!(run.status, JobStatus::Failed, "策略报错没有回写终态");
    assert_eq!(
        run.error_code.as_deref(),
        Some("STRATEGY_JOB_FAILED"),
        "失败运行必须带错误码，否则读侧只能看到状态"
    );
    assert_eq!(
        run.next_retry_ts, None,
        "默认 max_attempts=1 却写了 next_retry_ts，就是声明没人执行的重试"
    );
    assert_eq!(run.attempt, 1);
    // 失败不把作业从队列里拿走：条目留给租约过期后的重投/人工，worker 只负责不回自己。
    assert_eq!(
        case.queue.available(u64::MAX).unwrap().len(),
        1,
        "失败收口后队列条目不该消失"
    );
    assert!(
        !case
            .data
            .join("job-queue")
            .join("done")
            .join(format!("{run_id}.json"))
            .exists(),
        "没确认过的作业被搬进 done，等于宣称它跑完了"
    );
    let _ = std::fs::remove_dir_all(&case.dir);
}

/// 反向验证的基线：同一份夹具只把 instrument 换成合法值，运行仍留在 `Running`，作业就必须
/// 照常执行并回写成功 —— 证明上一条红的是终态判据，不是夹具。
#[test]
fn a_still_running_strategy_job_is_executed_and_succeeds() {
    let case = strategy_job_case("v13r2p12-running-baseline", false);
    let run_id = seed_run_and_enqueue(&case).unwrap();
    run_case(&case).expect("Running 运行必须照常执行，终态闸门不许把正常作业也拦掉");
    let state = case.store.load_scheduler_at(&case.state_path).unwrap();
    let run = state.run(run_id).cloned().unwrap();
    assert_eq!(run.status, JobStatus::Succeeded, "正常作业没有回写成功终态");
    assert!(run.error_code.is_none(), "成功运行的 error_code 必须为空");
    assert!(case.queue.available(u64::MAX).unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&case.dir);
}

/// 已收口的运行即使条目还在，也只能被确认并跳过：再执行一次就是二次提交。
/// 三种终态都过一遍，且 `error_code`/`attempt` 必须原样保留（闸门不得改写已有结局）。
#[test]
fn a_redelivered_final_run_is_acked_without_a_second_execution() {
    for (slug, success, code, expected, expected_error) in [
        ("succeeded", true, "ignored", JobStatus::Succeeded, None),
        (
            "failed",
            false,
            "STRATEGY_JOB_FAILED",
            JobStatus::Failed,
            Some("STRATEGY_JOB_FAILED"),
        ),
        (
            "intervention",
            false,
            "NEEDS_INTERVENTION",
            JobStatus::NeedsIntervention,
            Some("NEEDS_INTERVENTION"),
        ),
    ] {
        let case = strategy_job_case(&format!("v13r2p12-redelivery-{slug}"), true);
        let run_id = seed_run_and_enqueue(&case).unwrap();
        // 「回写终态之后、ack 之前掉电」的形态：条目带的是入队那一刻的 Running 副本，
        // 状态侧那条运行已经被改成终态。
        finalize_run(&case, run_id, success, if success { "none" } else { code });
        // 夹具自证：没有租约挡着，条目确实会被算作可领取。
        assert_eq!(case.queue.available(u64::MAX).unwrap().len(), 1);
        // 夹具的目标快照读不回：闸门若缺席，这一句会拿作业体的报错换掉「跳过」。
        run_case(&case).unwrap_or_else(|error| {
            panic!("{expected:?} 运行重投后仍被执行，终态闸门没有生效: {error}")
        });
        let state = case.store.load_scheduler_at(&case.state_path).unwrap();
        let run = state.run(run_id).cloned().unwrap();
        assert_eq!(run.status, expected, "{slug}：跳过路径改写了运行结局");
        assert_eq!(
            run.error_code.as_deref(),
            expected_error,
            "{slug}：结局错误码被换掉"
        );
        assert_eq!(run.attempt, 1, "{slug}：跳过不该记第二次尝试");
        assert!(
            case.queue.available(u64::MAX).unwrap().is_empty(),
            "{slug}：已终态的条目必须被确认，否则会永远卡在队列里"
        );
        assert!(
            case.data
                .join("job-queue")
                .join("done")
                .join(format!("{run_id}.json"))
                .exists(),
            "{slug}：确认走过 done，条目才真的离开队列"
        );
        let _ = std::fs::remove_dir_all(&case.dir);
    }
}

/// 终态判据的两条豁免边界：读不到运行按「未终态」处理（实时策略作业的运行从来不入 Scheduler
/// 状态，当成终态会让这类作业永远跑不了）；实时作业的失败回写同样豁免，而非实时作业缺运行
/// 必须报错，不能静默当成已收口。
#[test]
fn unknown_and_live_strategy_runs_stay_out_of_the_final_gate() {
    let case = strategy_job_case("v13r2p12-live-exempt", false);
    assert!(
        !strategy_run_is_final(&case.store, &case.state_path, 42_42_42).unwrap(),
        "Scheduler 里没有的运行不能被认成终态，否则实时策略作业永远跑不了"
    );
    fail_strategy_job_run(
        &case.store,
        &case.state_path,
        "live-strategy:whatever",
        42_42_42,
        1_700_000_000,
    )
    .expect("实时策略作业没有 Scheduler 侧运行，失败回写应与成功收口同一口径豁免");
    let error = fail_strategy_job_run(
        &case.store,
        &case.state_path,
        "strategy-terminal-case",
        42_42_42,
        1_700_000_000,
    )
    .expect_err("普通策略作业的运行不存在时必须报错，不能静默当成已收口");
    assert!(error.contains("UnknownRun"), "{error}");
    let _ = std::fs::remove_dir_all(&case.dir);
}

/// 退役的两颗零构造档位不许从线格式悄悄复活（`EventKind::Timer` 先例）。
///
/// `start_run` 直接建 `Running`（没有「已登记未开始」的构造点），暂停走的是 `StrategyState`
/// 而不是作业档位，所以 `JobStatus::Pending`/`Paused` 全仓零生产者、零入边，只给每个 `match`
/// 留一条永不为真的臂。删掉它们之后，一份写着旧档位的持久化状态必须**当场被拒**，而不是被
/// serde 默默读成一个别的档位 —— 后者会让「这条运行到底是什么状态」变成一个谁都没判过的值。
/// 源码侧由 `tools/check_architecture.py` 的 `scheduler_retry_honesty_check` 钉住不许回来。
#[test]
fn retired_job_status_variants_do_not_come_back_through_the_wire() {
    use qx_scheduler::Scheduler;
    // 现役词表就是这四颗；序列化名即变体名（`JobStatus` 没有 rename_all）。
    for (status, wire) in [
        (JobStatus::Running, "\"Running\""),
        (JobStatus::Succeeded, "\"Succeeded\""),
        (JobStatus::Failed, "\"Failed\""),
        (JobStatus::NeedsIntervention, "\"NeedsIntervention\""),
    ] {
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            wire,
            "现役档位的线格式名变了：读写两侧会各认一套"
        );
    }
    let case = strategy_job_case("v13audit6-retired-status", true);
    seed_run_and_enqueue(&case).unwrap();
    let encoded = case
        .store
        .load_scheduler_at(&case.state_path)
        .unwrap()
        .to_json()
        .unwrap();
    assert!(
        encoded.contains("\"Running\""),
        "夹具没有造出一条 Running 运行，本用例会对着空状态自证"
    );
    for retired in ["Pending", "Paused"] {
        let tampered = encoded.replace("\"Running\"", &format!("\"{retired}\""));
        assert!(
            Scheduler::from_json(&tampered).is_err(),
            "旧档位 {retired} 仍能从线格式读进 JobStatus"
        );
    }
    let _ = std::fs::remove_dir_all(&case.dir);
}
