//! 无键账户读模型必须讲同一个账户（V11 R12）。
//!
//! 账簿此前取配置里第一个账户 worker，全局兼容快照却被最后发布的那个账户覆盖：
//! 多账户部署下调用方没有选账户的余地，却拿到两份互不相认的读模型。这里钉住
//! 「无键的快照与无键的账簿说的是同一条账户」，以及「第二个账户仍按身份读得到」。

use super::*;

/// 无键端点必须讲同一个账户（V11 R12）：账簿此前取配置里第一个账户 worker，而全局兼容快照
/// 被**最后**发布的那个账户覆盖。多账户部署下调用方没有选账户的余地，却拿到两份互不相认的
/// 读模型——账簿说 A 的成交，快照说 B 的权益。
#[test]
fn unkeyed_read_models_describe_the_same_account() {
    let root = temp_cli_case_dir("api-default-account");
    let base = paper_runtime_config(&root);
    seed_paper_fill_with_fee(&root, &base);
    let main_log = paper_account_log();
    // 第二个账户域排在后面：同一份 paper worker 换个身份，日志里只有一条行情事实、没有成交。
    let mut config = paper_runtime_config(&root);
    let mut shadow = config
        .workers
        .iter()
        .find(|worker| owns_account_event_log(worker))
        .expect("paper 拓扑必须有账户 worker")
        .clone();
    shadow.id = "paper-execution-shadow".into();
    shadow.account_id = Some("shadow".into());
    config.workers.push(shadow);
    let shadow_log = account_event_log_name("shadow", "paper").expect("shadow 拼得出日志身份");
    let ts = runtime_timestamp_ms();
    let mut pipeline = LiveEventPipeline::open(&root, &shadow_log, "USDT").unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::market_quote(
            InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            QuoteTick::new(
                ts,
                Price::from_i64(99),
                Quantity::from_i64(1_000),
                Price::from_i64(100),
                Quantity::from_i64(1_000),
                ts,
            ),
            ts,
            ts,
            "default-account:quote",
        ))
        .unwrap();
    drop(pipeline);
    assert_eq!(
        default_account_event_log(&config, &root)
            .unwrap()
            .as_deref(),
        Some(main_log.as_str()),
        "默认账户是配置里第一个真有日志的账户 worker，不是最后一个"
    );

    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();
    let service = build_configured_api_service(&config, &config_path).unwrap();
    let snapshot: serde_json::Value =
        serde_json::from_str(&service.handle("GET", "/account/snapshot", "", ts).body).unwrap();
    let ledger: serde_json::Value =
        serde_json::from_str(&service.handle("GET", "/account/ledger", "", ts).body).unwrap();
    assert_eq!(snapshot["header"]["account_id"], "main");
    let entries = ledger.as_array().expect("/account/ledger 返回账簿条目数组");
    assert!(!entries.is_empty(), "默认账户 main 有成交，账簿不该是空的");
    for entry in entries {
        assert_eq!(
            entry["account_id"], "main",
            "无键快照说的是 main，无键账簿就必须同一条账户"
        );
    }
    // 不占全局位不等于丢掉账户：带身份的查询仍然读得到第二个账户。
    let keyed: serde_json::Value = serde_json::from_str(
        &service
            .handle(
                "GET",
                "/account/snapshot?account_id=shadow&venue_id=paper",
                "",
                ts,
            )
            .body,
    )
    .unwrap();
    assert_eq!(
        keyed["header"]["account_id"], "shadow",
        "第二个账户必须仍按身份读得到，实际 {keyed}"
    );
    let _ = std::fs::remove_dir_all(root);
}
