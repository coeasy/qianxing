//! 实时策略「这笔订单的依据还是不是生成时那根闭合 Bar」的闸门用例（V13 R2 收口 #210）。
//!
//! `live_strategy_job_is_stale` 是 live 链路上唯一挡住"按旧 Bar 下单"的地方：worker 在执行前
//! 与执行中各问一次，指纹一变就确认掉队列条目并跳过本轮。第十二遍把两处收口成一处之后，
//! 第十三遍的变异反向验证发现两个关键改动全是绿的 —— 把指纹比较改成恒等、把确认条目的时钟
//! 从租约域换成毫秒墙钟，整树 257 个用例一个都不红。换句话说：这段闸门自诞生起就没有用例，
//! 而它守的是"实盘下单依据"。本文件补齐，并钉住六条口径：
//! 1. 快照指纹变了 → 跳过，且条目真的被确认离开队列；
//! 2. 确认走 `lease_now`（秒）：拿毫秒墙钟确认会被自己的租约判过期，跳过路径直接变成报错；
//! 3. 指纹没变 → 放行执行，条目不得被确认掉；
//! 4. 不是实时策略作业（无期望指纹）→ 连快照都不读，快照坏了也照常放行；
//! 5. 行情 worker 掉线（快照文件没了）→ 按"依据已过期"跳过，不裸着下单；
//! 6. 快照本身不合法（instrument 对不上）→ 走 `Err` 让 worker 退出，不能混进"静默跳过本轮"。

use super::*;

const FRAME_INSTRUMENT: &str = "BTCUSDT.BINANCE";
const WORKER_ID: &str = "strategy-1";
/// 本轮 tick 的毫秒墙钟；`live_max_staleness_ms` 省略时按 3 个 1m 周期 = 180 秒。
const NOW_MS: u64 = 1_700_000_000_000;
const TIMEFRAME_MS: u64 = 60_000;
const LEASE_SECONDS: u64 = 30;

/// 写出一份三根 Bar 的闭合快照并返回它的指纹。
///
/// 最后一根 Bar 的收盘时间正好落在 `NOW_MS`，因此换 `close_delta` 只换"策略看到的那根 Bar 的
/// 内容"，不换"它新不新鲜" —— 用例里两条判据必须是可分开的，否则红的时候不知道输在哪一格。
fn write_closed_snapshot(path: &Path, close_delta: i128) -> u64 {
    let frame = BarFrame {
        instrument: InstrumentId::parse(FRAME_INSTRUMENT).expect("夹具 instrument 合法"),
        source: DataSourceId::new("snapshot-staleness-case"),
        ts: (0..3_u64)
            .map(|index| NOW_MS - (3 - index) * TIMEFRAME_MS)
            .collect(),
        open_raw: vec![100_000_000_000; 3],
        high_raw: vec![100_000_000_000; 3],
        low_raw: vec![100_000_000_000; 3],
        close_raw: vec![100_000_000_000 + close_delta; 3],
        volume_raw: vec![1_000_000_000; 3],
    };
    std::fs::write(path, frame.to_json()).expect("写入夹具快照失败");
    frame.digest()
}

struct StalenessCase {
    dir: PathBuf,
    snapshot: PathBuf,
    strategy: StrategyRuntimeConfig,
    queue: ConfiguredJobQueue,
    /// 队列/租约域时钟（秒），与 `lease_clock(NOW_MS)` 同一个数。
    lease_now: u64,
}

/// 一份只改了实时字段的运行时策略配置：走真实模板，避免夹具自造一份与生产不同的形状。
fn staleness_case(label: &str) -> StalenessCase {
    let dir = temp_cli_case_dir(label);
    let snapshot = dir.join("bars.json");
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let template = workspace_root
        .join("deploy")
        .join("qianxing.runtime.example.json");
    let mut config = read_runtime_config(&template).expect("读取运行时配置模板失败");
    config.strategy.live_enabled = true;
    config.strategy.live_timeframe = "1m".into();
    config.strategy.live_closed_only = true;
    config.strategy.live_max_staleness_ms = None;
    config.strategy.instrument = Some(FRAME_INSTRUMENT.into());
    config.strategy.bars_snapshot_path = Some(snapshot.to_string_lossy().into_owned());
    config.strategy.builtin_reference_instrument = None;
    config.strategy.builtin_reference_bars_snapshot_path = None;
    StalenessCase {
        queue: ConfiguredJobQueue::Files(FileJobQueue::new(dir.join("job-queue"))),
        lease_now: lease_clock(NOW_MS),
        strategy: config.strategy,
        snapshot,
        dir,
    }
}

/// 按 worker 的口径把一条实时策略作业放进队列并领取租约：入队 → `available` 取条目 → `claim`。
/// 指纹走生产生产者 `live_strategy_job`，因此条目缺 `manifest_digest` 那一格不可能是夹具自造。
fn seed_live_job(case: &StalenessCase, data_fingerprint: u64) -> (QueuedJob, JobLease) {
    let (job, run) =
        live_strategy_job(&case.strategy, WORKER_ID, data_fingerprint, NOW_MS, "paper");
    case.queue
        .enqueue(job, run, case.lease_now)
        .expect("写入夹具队列条目失败");
    let mut pending = case.queue.available(case.lease_now).unwrap();
    assert_eq!(pending.len(), 1, "夹具条目在租约前必须可领取");
    let queued = pending.remove(0);
    let lease = case
        .queue
        .claim(queued.run.run_id, WORKER_ID, case.lease_now, LEASE_SECONDS)
        .expect("领取夹具租约失败");
    (queued, lease)
}

/// 执行前那一次调用的口径：`digest_now` 用本轮 tick 的毫秒墙钟，`reason` 是 `stale-market-digest`。
fn gate_before_evaluation(
    case: &StalenessCase,
    queued: &QueuedJob,
    lease: &JobLease,
    expected_digest: Option<u64>,
) -> Result<bool, String> {
    live_strategy_job_is_stale(
        &StrategyJobLease {
            queue: &case.queue,
            queued,
            worker_id: WORKER_ID,
            fencing_token: lease.fencing_token,
            lease_now: case.lease_now,
        },
        &case.strategy,
        expected_digest,
        NOW_MS,
        "stale-market-digest",
        "确认过期实时 Strategy Job 失败",
    )
}

fn queue_record(case: &StalenessCase, run_id: u64, dir_name: &str) -> PathBuf {
    case.dir
        .join("job-queue")
        .join(dir_name)
        .join(format!("{run_id}.json"))
}

/// 快照指纹变了：这笔订单的依据已经不是生成时那根 Bar —— 跳过执行，并把条目确认掉。
///
/// 第二格是这条用例的真正目的：确认必须拿 `lease_now`（秒）。把毫秒墙钟喂进 `ack_at` 会让
/// `expires_ts <= now` 恒成立，跳过路径当场变成 `LeaseExpired` 报错，worker 直接退出。
#[test]
fn a_changed_snapshot_digest_skips_the_job_and_acks_its_entry() {
    let case = staleness_case("v13r2p13-stale-changed");
    let expected = write_closed_snapshot(&case.snapshot, 0);
    // 策略生成时看到的是收盘于 NOW_MS 的那根 Bar；此刻快照已被行情 worker 换掉。
    write_closed_snapshot(&case.snapshot, 1);
    let (queued, lease) = seed_live_job(&case, expected);
    let run_id = queued.run.run_id;

    let stale = gate_before_evaluation(&case, &queued, &lease, Some(expected))
        .expect("指纹变了应走跳过通道，而不是把队列故障报成闸门故障");
    assert!(stale, "快照指纹变了却没有判过期，等于允许按旧 Bar 下单");
    assert!(
        queue_record(&case, run_id, "done").exists(),
        "跳过的条目必须真的离开队列，否则下一轮会把它再领一次"
    );
    assert!(
        !queue_record(&case, run_id, "queue").exists(),
        "确认只搬不删：条目还留在 queue 就是没走完确认"
    );
    assert!(
        case.queue
            .available(case.lease_now + 31)
            .unwrap()
            .is_empty(),
        "已确认的条目在租约过期后也不该重新可见"
    );
    let _ = std::fs::remove_dir_all(&case.dir);
}

/// 反向基线：同一份夹具只把快照换回指纹相同的内容，闸门就必须放行且不碰条目 ——
/// 证明上一条红的是指纹判据，不是夹具或队列本身。
#[test]
fn an_unchanged_snapshot_digest_lets_the_job_execute() {
    let case = staleness_case("v13r2p13-stale-fresh");
    let expected = write_closed_snapshot(&case.snapshot, 0);
    let (queued, lease) = seed_live_job(&case, expected);
    let run_id = queued.run.run_id;

    let stale = gate_before_evaluation(&case, &queued, &lease, Some(expected))
        .expect("快照与指纹同源时闸门不该报错");
    assert!(!stale, "闭合 Bar 没变却被判过期，实时策略永远不会下单");
    assert!(
        !queue_record(&case, run_id, "done").exists(),
        "放行的条目被确认掉了，作业再没人执行"
    );
    assert_eq!(
        case.queue.available(case.lease_now + 31).unwrap().len(),
        1,
        "放行的条目必须留在队列里等本轮跑完或租约过期后重投"
    );
    let _ = std::fs::remove_dir_all(&case.dir);
}

/// 非实时策略作业没有期望指纹，闸门整段不该参与：快照坏成不可解析也必须放行。
/// 读一次快照再判 `None` 会把普通作业永久卡在队列里 —— 这条用例钉的是"先问是不是实时作业"。
#[test]
fn a_job_without_an_expected_digest_never_reads_the_snapshot() {
    let case = staleness_case("v13r2p13-stale-non-live");
    std::fs::write(&case.snapshot, "not a BarFrame at all").unwrap();
    let (queued, lease) = seed_live_job(&case, 0xabcd);
    let run_id = queued.run.run_id;

    let stale = gate_before_evaluation(&case, &queued, &lease, None).expect("非实时作业不该报错");
    assert!(!stale, "没有期望指纹的作业被快照闸门拦下了");
    assert!(
        !queue_record(&case, run_id, "done").exists(),
        "非实时作业的条目不归这条闸门确认"
    );
    let _ = std::fs::remove_dir_all(&case.dir);
}

/// 行情 worker 掉线（快照文件消失）时 `live_strategy_snapshot_digest` 回 `Ok(None)`：
/// 这与"指纹变了"是同一条结论 —— 依据已经不新鲜，跳过并确认，而不是拿上一轮的仓位依据裸跑。
#[test]
fn a_missing_snapshot_is_treated_as_stale() {
    let case = staleness_case("v13r2p13-stale-missing");
    let expected = write_closed_snapshot(&case.snapshot, 0);
    std::fs::remove_file(&case.snapshot).unwrap();
    let (queued, lease) = seed_live_job(&case, expected);
    let run_id = queued.run.run_id;

    let stale = gate_before_evaluation(&case, &queued, &lease, Some(expected))
        .expect("快照缺失应走跳过通道");
    assert!(
        stale,
        "读不到闭合 Bar 却放行，等于让策略在没有行情依据时下真实单"
    );
    assert!(
        queue_record(&case, run_id, "done").exists(),
        "跳过的条目同样要确认掉，否则每轮都会重演一遍"
    );
    let _ = std::fs::remove_dir_all(&case.dir);
}

/// 快照与策略配置对不上是**配置/数据不合法**，与"这一轮不该跑"必须分通道：`Err` 让 worker 退出，
/// 绝不能被收成一次静默跳过 —— 否则整条实时链会安静地一单不下，看起来还在跑。
#[test]
fn an_inconsistent_snapshot_instrument_fails_the_worker() {
    let case = staleness_case("v13r2p13-stale-mismatch");
    let expected = write_closed_snapshot(&case.snapshot, 0);
    let mut config = case.strategy.clone();
    config.instrument = Some("ETHUSDT.BINANCE".into());
    let (queued, lease) = seed_live_job(&case, expected);
    let run_id = queued.run.run_id;

    let error = live_strategy_job_is_stale(
        &StrategyJobLease {
            queue: &case.queue,
            queued: &queued,
            worker_id: WORKER_ID,
            fencing_token: lease.fencing_token,
            lease_now: case.lease_now,
        },
        &config,
        Some(expected),
        NOW_MS,
        "stale-market-digest",
        "确认过期实时 Strategy Job 失败",
    )
    .expect_err("instrument 不一致是数据不合法，不能被当成跳过本轮");
    assert!(error.contains("instrument 不一致"), "报错丢了身份: {error}");
    assert!(
        !queue_record(&case, run_id, "done").exists(),
        "报错的条目不该被这条闸门确认掉，它得留给租约过期后的重投或人工"
    );
    let _ = std::fs::remove_dir_all(&case.dir);
}

/// 确认条目自己失败（租约已被别的 worker 接管）时，错误必须带上**调用点**的报错语：
/// 执行前与执行中两处闸门共用同一段收口，读侧要靠这两句话分清是哪一阶段丢了租约。
#[test]
fn an_ack_failure_is_reported_with_the_call_sites_message() {
    let case = staleness_case("v13r2p13-stale-ack-failure");
    let expected = write_closed_snapshot(&case.snapshot, 0);
    write_closed_snapshot(&case.snapshot, 2);
    let (queued, lease) = seed_live_job(&case, expected);
    // 租约过期后被第二个 worker 接管：原 worker 的 fencing token 失效，确认必被拒。
    case.queue
        .claim(
            queued.run.run_id,
            "strategy-2",
            case.lease_now + 31,
            LEASE_SECONDS,
        )
        .expect("过期租约应可被接管");

    let error = gate_before_evaluation(&case, &queued, &lease, Some(expected))
        .expect_err("确认失败的条目不能装作已经跳过");
    assert!(
        error.contains("确认过期实时 Strategy Job 失败"),
        "报错丢了调用点身份: {error}"
    );
    assert!(
        error.contains("Unauthorized") || error.contains("fencing"),
        "报错丢了队列侧的失败原因: {error}"
    );
    let _ = std::fs::remove_dir_all(&case.dir);
}
