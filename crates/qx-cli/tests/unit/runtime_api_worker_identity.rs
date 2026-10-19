//! `serve` 必须按 role 找到它的 API worker，而不是按字面量名字猜（V11 S1）。

use super::*;

/// 合法拓扑，只是把 API worker 改了个名：worker id 是自由命名，配置校验只数"启用的
/// api role worker 恰好一个"，不看名字。
fn renamed_api_topology(data_dir: &Path) -> RuntimeConfig {
    let mut config = paper_runtime_config(data_dir);
    // 这份配置是被用例改过的内存拓扑，不是锁定过的发布配置。
    config.config_fingerprint = None;
    for worker in config.workers.iter_mut() {
        if worker.role == WorkerRole::Api {
            worker.id = "api-gw".into();
        }
    }
    config
}

/// 字面量 `"api"` 在这份合法配置上就是"启动即死"：监督器的注册表按 `worker.id` 建键，
/// 按名字 spawn 会被判未知 worker。这条用例把两件事一起钉住——按 role 解析出的 id 能
/// 注册，而旧的字面量不能，于是它同时是缺陷本体的证明。
#[test]
fn renamed_api_worker_is_the_one_the_supervisor_accepts() {
    let root = temp_cli_case_dir("runtime-api-worker-identity");
    let config = renamed_api_topology(&root);
    let supervisor =
        RuntimeSupervisor::new(config.clone()).expect("只改 api worker 的 id 不该让拓扑校验失败");
    let resolved =
        configured_api_worker_id(supervisor.config()).expect("启用的 api worker 解析得出 id");
    assert_eq!(resolved, "api-gw", "必须按 role 解析，而不是回落到字面量");

    assert!(
        supervisor.spawn_worker("api", |_| Ok(())).is_err(),
        "字面量 api 在这份拓扑上必须失败，否则这条用例咬不住缺陷本体"
    );
    let by_role = supervisor
        .spawn_worker(&resolved, |_| Ok(()))
        .expect("按 role 解析出的 id 必须能注册");
    let _ = by_role.join();
}

/// 关掉了就没有"那一个"：解析必须报错，不能静默挑别的 worker 当 API。
#[test]
fn api_worker_lookup_fails_closed_without_an_enabled_one() {
    let root = temp_cli_case_dir("runtime-api-worker-disabled");
    let mut config = renamed_api_topology(&root);
    for worker in config.workers.iter_mut() {
        if worker.role == WorkerRole::Api {
            worker.enabled = false;
        }
    }
    let error = configured_api_worker_id(&config).unwrap_err();
    assert!(
        error.contains("api worker"),
        "报错要说清缺的是 api worker：{error}"
    );
}

/// #164：`serve` 的 API worker 现在交给停机阶梯汇合，因此阶梯必须真的等线程收尾。
/// 这条挡"慢半拍收尾"的形状：worker 线程还在跑最后 50ms 时，汇合点不能提前返回。
#[test]
fn join_worker_handle_waits_for_the_worker_thread_to_finish_its_tail() {
    let root = temp_cli_case_dir("runtime-api-shutdown-ladder");
    let config = paper_runtime_config(&root);
    let supervisor = RuntimeSupervisor::new(config).expect("纸面拓扑应能建监督器");
    let id = configured_api_worker_id(supervisor.config()).expect("模板里有启用的 api worker");
    let tail_written = Arc::new(AtomicBool::new(false));
    let flag = tail_written.clone();
    let handle = supervisor
        .spawn_worker(&id, move |_context| {
            thread::sleep(Duration::from_millis(50));
            flag.store(true, Ordering::SeqCst);
            Ok(())
        })
        .expect("按 role 解析出的 id 应能注册");
    join_worker_handle(&supervisor, handle, "API", &id).expect("正常收尾的 worker 不该报错");
    assert!(
        tail_written.load(Ordering::SeqCst),
        "停机阶梯没等 worker 线程跑完尾部就返回，优雅停机仍不可达"
    );
}

/// #164 的收口判据：worker 的返回价值只存在于 JoinHandle 里，摘掉 `handle.join()`
/// 就没别的途径能把它交给调用方。因此这条用例咬得住"只等标志就返回"的变异，
/// 而上一条（尾部可见性）挡不住——原子写在退出前，轮询侧几乎总能看见。
#[test]
fn join_worker_handle_propagates_the_worker_error_instead_of_swallowing_it() {
    let root = temp_cli_case_dir("runtime-api-shutdown-error");
    let supervisor =
        RuntimeSupervisor::new(paper_runtime_config(&root)).expect("纸面拓扑应能建监督器");
    let id = configured_api_worker_id(supervisor.config()).expect("模板里有启用的 api worker");
    let handle = supervisor
        .spawn_worker(&id, |_| Err("API 服务停止：监听套接字已关闭".into()))
        .expect("按 role 解析出的 id 应能注册");
    let error = join_worker_handle(&supervisor, handle, "API", &id)
        .expect_err("worker 以错误收摊时，serve 必须把它当成自己的失败");
    assert!(
        error.contains("监听套接字已关闭"),
        "报错要带 worker 自己的原因，而不是只说线程结束了: {error}"
    );
}

/// #164 的调用点：`serve` 的两条分支（明文 API 与 mTLS API）都必须经停机阶梯汇合，
/// 而不是各自 `worker.join()`。少一条分支就是"另一半 serve 路径仍然停不下来"，
/// 而这类漏接在行为上只有真按 Ctrl+C 才看得见，所以在这里按源码点名。
#[test]
fn both_serve_branches_join_the_api_worker_through_the_shutdown_ladder() {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("strategy_contract.rs"),
    )
    .unwrap();
    for role in ["\"API\"", "\"mTLS API\""] {
        let call = format!("join_worker_handle(&supervisor, worker, {role}, &api_worker_id)");
        assert_eq!(
            text.matches(&call).count(),
            1,
            "serve 的 {role} 分支不再经停机阶梯汇合 API worker"
        );
    }
}

/// #164 的另一半：投影桥线程的收摊必须置空句柄并 join。少了 join 时尾部写入看不见，
/// 少了置空时第二次调用会 panic —— 两条断言分别挡住"漏停"和"重复停"。
#[test]
fn stop_api_projection_bridge_joins_and_clears_the_projection_thread() {
    let stop = Arc::new(AtomicBool::new(false));
    let tail_written = Arc::new(AtomicBool::new(false));
    let flag = tail_written.clone();
    let polled = stop.clone();
    let mut projection = Some(thread::spawn(move || {
        while !polled.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(2));
        }
        flag.store(true, Ordering::SeqCst);
    }));
    stop_api_projection_bridge(&stop, &mut projection);
    assert!(
        tail_written.load(Ordering::SeqCst),
        "投影线程没被 join，serve 退出时它还在写 EventLog"
    );
    assert!(
        projection.is_none(),
        "句柄没置空，第二次收摊会重复 join 同一个线程"
    );
}

/// #221 的装配侧：`serve` 的两条分支交给 API 的必须是**那只钟本身**，不是提前取好的一份值。
///
/// 立案现场是签名之前：两条分支传的是 `runtime_timestamp_ms()` 的结果，而 `serve` 把它按连接
/// 复用，于是限流桶、审计时间、命令租约三条通道在进程生命周期里共用同一个戳。签名换成
/// `impl FnMut() -> u64` 之后，传一个 `u64` 已经编译不过，但**换一只冻住的钟**（在闭包里捕获
/// 一个常数）类型上照样合法——那只有长跑进程才看得见，所以按源码点名。
///
/// 口径如实写出：这条判据钉的是"传的是函数、且每条分支只有一处"这种最简形态。改成
/// `|| runtime_timestamp_ms()` 那种等价写法会红在这里，那是有意的摩擦——两条分支写法一致，
/// 运维读一处就知道另一处。行为侧（每接受一条连接现取一次）由
/// `crates/qx-api/tests/request_timestamp_clock.rs` 用真 socket 钉。
#[test]
fn both_serve_branches_pass_a_live_clock_to_the_api_worker() {
    let text = workspace_source("crates/qx-cli/src/strategy_contract.rs");
    let stripped = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>();
    for call in [
        "serve(listener,runtime_timestamp_ms,",
        "serve_tls_mtls_with_stores(listener,&store,&identity_store,runtime_timestamp_ms,",
    ] {
        assert_eq!(
            stripped.matches(call).count(),
            1,
            "serve 的某一条分支不再把 `runtime_timestamp_ms` 本身交给 API worker（点名的形态：\
             {call}）。传它的结果、或在闭包里捕获一份常数，都是把那三条通道冻在启动那一刻。"
        );
    }
}
