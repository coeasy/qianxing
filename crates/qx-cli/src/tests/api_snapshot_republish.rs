//! 读模型的快照半边必须跟着事件半边一起刷新（V11 G1）。
//!
//! 投影桥每 250 毫秒重投影一次账户事件，账户快照却只在 `build_configured_api_service`
//! 装载一次：服务进程是按天跑的，`/account/snapshot` 于是把启动那一刻的权益一路念下去，
//! 而同一个读模型的事件游标一直在往前走——两半讲出两个时点。

use super::*;

/// boot 之后往 paper 账户日志再记一笔转入：读模型唯一的"账户状态变了"来源。
fn seed_extra_paper_transfer(data_dir: &Path, amount: i64) {
    let mut pipeline = LiveEventPipeline::open(data_dir, paper_account_log(), "USDT").unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountCashflow {
                cashflow: AccountCashflow {
                    account_id: "main".into(),
                    venue_id: "paper".into(),
                    currency: "USDT".into(),
                    kind: CashflowKind::Transfer,
                    amount: Money::from_i64(amount),
                    external_id: format!("republish:transfer:{amount}"),
                },
            },
            1,
            1,
            1,
            // 相关号进 dedup_key：两轮都用同一个的话，第二笔会被当成重复事实吞掉，
            // 用例就变成了"等了个根本没发生的改动"。
            format!("republish:transfer:{amount}").as_str(),
        ))
        .unwrap();
}

/// 装载一次就推进一次：带键投影与无键那一格必须同步换到新状态。
///
/// 无键快照由 `publish_snapshot` 顺带镜像进账户投影，反向不成立；桥若改用只写投影的那一侧，
/// `/account/snapshot` 会独自留在旧时点而带键查询已经前进（V11 R12 的那条规则在这里再钉一次）。
#[test]
fn account_snapshots_reload_the_state_written_after_boot() {
    let root = temp_cli_case_dir("api-snapshot-republish");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_runtime_config(&data_dir);
    seed_paper_fill_with_fee(&data_dir, &config);

    let mut state = ApiState::default();
    assert!(
        publish_api_account_snapshots(&mut state, &config).unwrap() >= 1,
        "paper 拓扑至少要装出一个账户的快照"
    );
    let seeded = state
        .account_snapshot_for("main", "paper")
        .expect("boot 装载必须建出 main/paper 的快照");
    let cash = seeded.cash_raw.get("USDT").copied().unwrap_or_default();
    assert_eq!(
        state.snapshot.as_ref().map(|s| s.state_hash()),
        Some(seeded.state_hash()),
        "无键那一格与带键投影说的是同一份状态"
    );

    seed_extra_paper_transfer(&data_dir, 5_000);
    assert!(publish_api_account_snapshots(&mut state, &config).unwrap() >= 1);
    let reloaded = state
        .account_snapshot_for("main", "paper")
        .expect("重装载之后投影仍在");
    assert_eq!(
        reloaded.cash_raw.get("USDT").copied(),
        Some(cash + 5_000 * SCALE),
        "boot 之后落进账户日志的事实必须被重装载读出来，而不是停在启动那一刻"
    );
    assert_eq!(
        state.snapshot.as_ref().map(|s| s.state_hash()),
        Some(reloaded.state_hash()),
        "重装载只推进带键投影、留下无键那一格，等于同一份读模型两个时点"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 端到端证据：真的把投影桥起起来，boot 之后写入的事实要在有界时间内出现在 HTTP 快照里。
///
/// 两轮变化是刻意的：起桥之前那一轮挡"只在 boot 装一次"，起桥之后那一轮挡"只在桥启动时
/// 多装一次"（M1b）。只留前一轮的话，后者会因为启动那次正好读到新状态而长期混过断言。
#[test]
fn api_projection_bridge_republishes_snapshots_until_the_endpoint_moves() {
    let root = temp_cli_case_dir("api-snapshot-bridge");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_runtime_config(&data_dir);
    seed_paper_fill_with_fee(&data_dir, &config);
    let config_path = data_dir.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();
    let service = build_configured_api_service(&config, &config_path).unwrap();
    let boot = service
        .account_snapshot_for("main", "paper")
        .expect("boot 装载过 paper 账户快照");

    seed_extra_paper_transfer(&data_dir, 5_000);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let bridge =
        spawn_api_projection_bridge(&config, service.clone(), std::sync::Arc::clone(&stop))
            .expect("paper 拓扑有账户日志，投影桥必须启动");
    let reloaded = wait_for_snapshot_change(&service, boot.state_hash())
        .expect("投影桥必须把 boot 之前的账户变化重装进读模型");
    // 桥已经在跑了，这一笔只有"每轮都重装"读得到。
    seed_extra_paper_transfer(&data_dir, 3_000);
    let second = wait_for_snapshot_change(&service, reloaded.state_hash())
        .expect("投影桥必须每轮重装，而不是只在启动时装一次");
    stop.store(true, std::sync::atomic::Ordering::Release);
    bridge.join().expect("投影桥线程要能收尾");

    let ts = runtime_timestamp_ms();
    let body: serde_json::Value =
        serde_json::from_str(&service.handle("GET", "/account/snapshot", "", ts).body).unwrap();
    assert_eq!(
        body["header"]["state_hash"].as_u64(),
        Some(second.state_hash()),
        "HTTP 侧读到的必须就是用例观察到的那份状态"
    );
    let expected = second.cash_raw.get("USDT").copied().unwrap_or_default();
    assert_eq!(
        body["cash_raw"]["USDT"].as_i64(),
        i64::try_from(expected).ok(),
        "现金格要跟着重装载前进，而不是留在 boot 那一版"
    );
    assert_eq!(
        second.cash_raw.get("USDT").copied(),
        boot.cash_raw
            .get("USDT")
            .copied()
            .map(|value| value + 8_000 * SCALE),
        "两轮转入都要被读出来：只装一次的修法只会停在第一轮的 +5000"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 轮询到有界超时为止，而不是定长 sleep：慢机器上定长等待只会把用例变成假失败。
fn wait_for_snapshot_change(service: &ApiService, previous: u64) -> Option<AccountSnapshot> {
    for _ in 0..200 {
        let snapshot = service.account_snapshot_for("main", "paper")?;
        if snapshot.state_hash() != previous {
            return Some(snapshot);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    None
}
