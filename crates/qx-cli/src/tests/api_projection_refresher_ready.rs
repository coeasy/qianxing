//! 投影桥不再刷新时，账户读模型必须自己在 `/ready` 上说出来（V11 I1）。
//!
//! G1 的"每轮重装"全部住在那个线程里，而它有两条启动失败的路径（账户 EventLog 列举失败、
//! `PipelineStorage::from_config` 失败）加一条"拓扑里没有账户级 EventLog"。桥没跑或已经退出，
//! `/account/snapshot` 那一族就停在最后一轮——而 `/ready` 此前只看投影自带的健康位，boot
//! 装进来的那一份永远是健康的，于是把"再也不会刷新"念成 Ready（与 R5 同一形状）。

use super::*;

fn readiness(service: &ApiService) -> (u16, String) {
    let response = service.handle("GET", "/ready", "", runtime_timestamp_ms());
    let body: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    (
        response.status,
        body["detail"].as_str().unwrap_or_default().to_string(),
    )
}

/// 真把桥起起来再让它退出：退出之后 `/ready` 必须念出"刷新者已停"并带上原因。
///
/// 停机那一支由线程内的守卫负责，不是外层 `join`——外层 join 发生在服务已经不接请求之后，
/// 那时候改这格状态已经没有读者。
#[test]
fn ready_reports_a_read_model_that_nobody_refreshes_anymore() {
    let root = temp_cli_case_dir("api-projection-refresher-ready");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_runtime_config(&data_dir);
    seed_paper_fill_with_fee(&data_dir, &config);
    let config_path = data_dir.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();
    let service = build_configured_api_service(&config, &config_path).unwrap();

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let bridge =
        spawn_api_projection_bridge(&config, service.clone(), std::sync::Arc::clone(&stop))
            .expect("paper 拓扑有账户日志，投影桥必须启动");
    let (status, detail) = readiness(&service);
    assert!(
        !detail.contains("projection_refresher_stopped"),
        "桥正在跑时不该出现刷新者停摆的判定（status={status} detail={detail}）"
    );

    stop.store(true, std::sync::atomic::Ordering::Release);
    bridge.join().expect("投影桥线程要能收尾");
    let (status, detail) = readiness(&service);
    assert_eq!(status, 503, "读模型不再刷新却报 Ready：{detail}");
    assert!(
        detail.contains("projection_refresher_stopped"),
        "停摆要说得出是停摆，而不是只报健康：{detail}"
    );
    assert!(
        detail.contains("投影桥线程已退出"),
        "原因要跟着一起报出来：{detail}"
    );
    let _ = std::fs::remove_dir_all(root);
}
