/// 调度链的最后一公里：作业 owner 与启用 Strategy worker 的连通性（V11 §41 E7）。
use super::*;

fn paper_config(data_dir: &Path) -> RuntimeConfig {
    let mut config = paper_runtime_config(data_dir);
    // 示例作业清单里的规格路径相对 data_dir，落在临时目录时改用绝对路径读不到，
    // 但 owner 判定只读 workers 与 jobs，先把 jobs_path 指到用例自己写的那份。
    config.scheduler.jobs_path = data_dir.join("jobs.json").to_string_lossy().into_owned();
    config
}

/// 一份合法作业的 JSON 骨架。`input_refs`/`output_refs`/`permission_scope` 三格是
/// `JobSpec` 的**必填**格（它们今天零生产读者，按 2026-10-06 方案 §13.2 保留不删并
/// 登记为 limitation，见 `zero_reader_fields.rs`），所以夹具必须照抄，否则连解析都过不去。
fn job_json(job_id: &str, owner: &str) -> serde_json::Value {
    serde_json::json!({
        "job_id": job_id,
        "job_version": "v1",
        "owner": owner,
        "enabled": true,
        // 注册表派发只认 `Cron` + `Any` + 一次尝试：这三样不齐的作业在装配处
        // 就当场拒（见 `undispatchable_trigger_shape_fails_closed_at_assembly` 那颗用例），
        // 所以 owner 路由的夹具必须声明一份真派发得到的形状。
        "trigger": { "Cron": "0 9 * * *" },
        "window": "Any",
        "depends_on": [],
        "input_refs": ["dataset:owner-routing"],
        "output_refs": ["owner-routing-report"],
        "timeout_seconds": 60,
        "retry_policy": {
            "max_attempts": 1,
            "backoff_seconds": 0,
            "retryable_codes": []
        },
        "concurrency_key": job_id,
        "idempotency_key": format!("{job_id}-v1"),
        "permission_scope": "strategy",
        "audit_reason": "owner routing case",
        "dry_run": true
    })
}

fn write_jobs(data_dir: &Path, owners: &[&str]) {
    let jobs = owners
        .iter()
        .enumerate()
        .map(|(index, owner)| job_json(&format!("job-{index}"), owner))
        .collect::<Vec<_>>();
    std::fs::write(
        data_dir.join("jobs.json"),
        serde_json::to_vec_pretty(&jobs).unwrap(),
    )
    .unwrap();
}

fn loaded(config: &RuntimeConfig, data_dir: &Path) -> Result<Vec<String>, String> {
    // jobs_path 已被 paper_config 换成绝对路径，这里只需要一个同目录的运行时文件锚点。
    let path = data_dir.join("runtime.json");
    let (_, scheduler, _) = load_scheduler_state(config, data_dir, &path)?;
    Ok(scheduler.jobs().map(|job| job.owner.clone()).collect())
}

/// 仓库自带的默认拓扑必须真的跑通调度链：example 作业清单用的是通配 owner，
/// 而该拓扑里确实有启用的 Strategy worker 接得住（V11 §41 E7 的原始现场）。
#[test]
fn shipped_example_topology_routes_its_wildcard_jobs() {
    let root = temp_cli_case_dir("scheduler-owner-shipped");
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.example.json");
    let mut config = read_runtime_config(&template).expect("默认示例拓扑必须可解析");
    config.storage.data_dir = root.to_string_lossy().into_owned();
    // 装配本身就会跑 owner 路由判定：这里不再抄一份判据自证，只读生产结论。
    let (_, scheduler, _) = load_scheduler_state(&config, &root, &template)
        .expect("默认示例的每个作业 owner 都必须有启用的 Strategy worker 领取");
    let owners = scheduler
        .jobs()
        .map(|job| job.owner.clone())
        .collect::<Vec<_>>();
    assert!(!owners.is_empty(), "默认示例作业清单不该为空");
    assert!(
        owners
            .iter()
            .any(|owner| owner == qx_scheduler::JOB_OWNER_ANY),
        "示例作业清单靠通配 owner 示范共享清单，改掉它等于关掉这条链的入口：{owners:?}"
    );
}

/// owner 指向一个不存在的 worker：过去只有 `JobSpec::validate` 的"非空"判据，
/// 于是作业入队后永无人领取，而 JobRun 已经是 Running、命令面打印 READY processed=0。
#[test]
fn unroutable_job_owner_fails_closed_instead_of_quietly_never_running() {
    let root = temp_cli_case_dir("scheduler-owner-typo");
    let config = paper_config(&root);
    write_jobs(&root, &["strategy-nobody"]);
    let error = loaded(&config, &root).expect_err("无人领取的 owner 必须失败");
    assert!(error.contains("job-0"), "{error}");
    assert!(error.contains("strategy-nobody"), "{error}");
    assert!(error.contains("无人领取"), "{error}");

    // 被拒绝的拓扑不得留下调度状态文件，否则下一次运行读的还是这份非法装配。
    assert!(
        !root.join(config.scheduler.state_path.as_str()).exists(),
        "拒绝装配时不应写出 scheduler 状态文件"
    );

    // 换一个干净的临时目录：owner 命中启用的 worker 时必须放行。
    let matched = temp_cli_case_dir("scheduler-owner-match");
    let matching = paper_config(&matched);
    write_jobs(&matched, &["strategy-paper"]);
    assert_eq!(
        loaded(&matching, &matched).expect("owner 命中启用 worker 必须放行"),
        vec!["strategy-paper".to_string()]
    );

    // 禁用唯一的 Strategy worker 与把 owner 改错是同一件事：判据看"启用且角色对"。
    let mut disabled = paper_config(&matched);
    for worker in disabled.workers.iter_mut() {
        if worker.role == WorkerRole::Strategy {
            worker.enabled = false;
        }
    }
    let error = loaded(&disabled, &matched).expect_err("没有启用的 Strategy worker 时不得放行");
    assert!(
        error.contains("该拓扑没有启用的 Strategy worker"),
        "{error}"
    );
}

/// 通配 owner 交给任意启用的 Strategy worker：一份共享作业清单可以服务多套拓扑。
#[test]
fn wildcard_owner_is_routable_only_when_someone_listens() {
    let root = temp_cli_case_dir("scheduler-owner-wildcard");
    let mut config = paper_config(&root);
    write_jobs(&root, &[qx_scheduler::JOB_OWNER_ANY]);
    let owners = loaded(&config, &root).expect("通配 owner 应被启用的 Strategy worker 接住");
    assert_eq!(owners, vec![qx_scheduler::JOB_OWNER_ANY.to_string()]);

    config
        .workers
        .retain(|worker| worker.role != WorkerRole::Strategy);
    let error = loaded(&config, &root).expect_err("没有领取者时通配 owner 同样是死队列");
    assert!(
        error.contains("该拓扑没有启用的 Strategy worker"),
        "{error}"
    );
}

/// 未启用的作业不该为拓扑负责：它不会被入队，也就不需要有人领取。
#[test]
fn disabled_jobs_are_not_required_to_be_routable() {
    let root = temp_cli_case_dir("scheduler-owner-disabled");
    let config = paper_config(&root);
    let mut payload = job_json("retired", "strategy-retired");
    payload["enabled"] = serde_json::Value::Bool(false);
    payload["trigger"] = serde_json::Value::String("Manual".into());
    std::fs::write(
        root.join("jobs.json"),
        serde_json::to_vec_pretty(&[payload]).unwrap(),
    )
    .unwrap();
    let path = root.join("runtime.json");
    let (_, scheduler, _) =
        load_scheduler_state(&config, &root, &path).expect("禁用作业的 owner 不参与路由判定");
    assert_eq!(scheduler.jobs().count(), 1);
}

/// 状态文件是上一轮装配留下的产物：换了 worker 名单再读它，owner 路由必须重判一次。
/// 少了这一问，作业会继续投进一条已经没人订阅的队列。
#[test]
fn loaded_state_is_revalidated_against_the_current_topology() {
    let root = temp_cli_case_dir("scheduler-owner-reload");
    let config = paper_config(&root);
    write_jobs(&root, &["strategy-paper"]);
    assert_eq!(
        loaded(&config, &root).expect("首轮装配应通过"),
        vec!["strategy-paper".to_string()]
    );
    assert!(
        root.join(config.scheduler.state_path.as_str()).exists(),
        "首轮装配应落盘调度状态"
    );

    let mut moved_on = paper_config(&root);
    for worker in moved_on.workers.iter_mut() {
        if worker.role == WorkerRole::Strategy {
            worker.enabled = false;
        }
    }
    let error = loaded(&moved_on, &root).expect_err("载入既有状态时同样要核对 owner");
    assert!(
        error.contains("该拓扑没有启用的 Strategy worker"),
        "{error}"
    );
}

/// 作业文件里的触发形状必须落在运行时派发真走得到的那一面上：非 `Cron` 触发器、非
/// `Any` 窗口、`max_attempts > 1` 今天在注册表里**什么都不触发**，而作业文件只按字段
/// 校验，于是"看起来合法、永远不跑"会一路活到启动（V11 N4）。三格各自单独拒一次：
/// 少挡任何一格，就还剩一种声明会静默失效。
#[test]
fn undispatchable_trigger_shape_fails_closed_at_assembly() {
    let cases = [
        ("trigger", "Manual", "只跑 Cron 触发且 window=Any", "Manual"),
        (
            "window",
            "Session",
            "只跑 Cron 触发且 window=Any",
            "Session",
        ),
        ("attempts", "3", "重试没有派发者", "max_attempts=3"),
    ];
    for (field, value, needle, offender) in cases {
        let root = temp_cli_case_dir(&format!("scheduler-trigger-{field}"));
        let config = paper_config(&root);
        let mut job = job_json("shape", "strategy-paper");
        match field {
            "trigger" => job["trigger"] = serde_json::Value::String(value.into()),
            "window" => job["window"] = serde_json::Value::String(value.into()),
            _ => {
                job["retry_policy"]["max_attempts"] =
                    serde_json::Value::from(value.parse::<u64>().unwrap())
            }
        }
        std::fs::write(
            root.join("jobs.json"),
            serde_json::to_vec_pretty(&[job]).unwrap(),
        )
        .unwrap();
        let error = loaded(&config, &root)
            .expect_err("派发不到的触发声明必须在装配当场拒，而不是静留在作业文件里");
        assert!(error.contains("shape"), "{field} => {error}");
        assert!(error.contains(needle), "{field} => {error}");
        // 报错还得点名是**哪一格**声明越界，否则运维只知道"这份文件不合法"。
        assert!(error.contains(offender), "{field} => {error}");
        assert!(
            !root.join(config.scheduler.state_path.as_str()).exists(),
            "{field}: 拒绝装配时不应写出调度状态文件"
        );
    }

    // 正对照：三格都齐的形状必须放行，否则上面的红只是"改什么都红"。
    let ok = temp_cli_case_dir("scheduler-trigger-ok");
    let ok_config = paper_config(&ok);
    write_jobs(&ok, &["strategy-paper"]);
    assert_eq!(
        loaded(&ok_config, &ok).expect("Cron + Any + 一次尝试必须放行"),
        vec!["strategy-paper".to_string()]
    );
}
