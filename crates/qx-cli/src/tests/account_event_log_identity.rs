//! 账户级 EventLog 身份的唯一构造点用例。
//!
//! 一个账户在一个 Venue 上只有一本事实账。写入端（user-stream / execution /
//! spread-recovery / reconciler）和读取端（API 投影、策略上下文、行情桥）各自
//! 拼一次名字，同一账户就会分裂成互不相认的几本账：`paper-submit-order` 曾经写
//! `paper-events`，而 worker 主链路读写 `paper-main-paper-events`；CCXT 侧命名还
//! 漏了 paper 分支，缺身份时用 `unknown` 兜底。兜底名写下的事实，读取端按账户身份
//! 永远找不到，等于静默丢账。

use super::*;

/// 身份键的规范化只在唯一构造点做一次：trim + Venue 大小写。
#[test]
fn account_identity_normalizes_its_key_in_one_place() {
    assert_eq!(
        account_event_log_name("main", "paper").as_deref(),
        Some("paper-main-paper-events"),
        "Paper 虚拟执行域的日志名即账户身份"
    );
    assert_eq!(
        account_event_log_name(" main ", "Paper").as_deref(),
        account_event_log_name("main", "paper").as_deref(),
        "大小写与空白不得把一个账户拆成两本账"
    );
    assert_eq!(
        account_event_log_name("main", " OKX ").as_deref(),
        Some("ccxt-main-okx-events")
    );
    assert_eq!(
        account_event_log_name("main", "Binance").as_deref(),
        Some("binance-main-binance-events")
    );
    assert_eq!(
        account_event_log_name("", "paper"),
        None,
        "空账户不是合法身份"
    );
    assert_eq!(
        account_event_log_name("main", "   "),
        None,
        "空 Venue 不是合法身份，也不能退回 paper"
    );
}

/// 需要账户日志的写入端都从同一身份派生名字，paper venue 走 CCXT 入口时也一样；
/// 行情事实按 worker 各自成册，与账户身份并行且互不遮蔽。
#[test]
fn every_account_log_writer_derives_the_same_name() {
    let paper = mk_worker("paper-execution", WorkerRole::Execution, "Paper", None);
    assert_eq!(
        required_account_event_log(&paper).as_deref(),
        Ok("paper-main-paper-events"),
        "paper venue 不能再被 CCXT 入口拼成 ccxt- 前缀"
    );
    let binance = mk_worker("binance-exec", WorkerRole::Execution, "BINANCE", None);
    assert_eq!(
        binance_event_log_name(&binance).as_deref(),
        Ok("binance-main-binance-events"),
        "Binance 执行 worker 与账户身份同名，Venue 大小写不另起一册"
    );
    let market = mk_worker("binance-market", WorkerRole::MarketData, "binance", None);
    assert_eq!(
        binance_event_log_name(&market).as_deref(),
        Ok("binance-market-events"),
        "行情 worker 不属于任何账户，按 worker 成册"
    );
    assert_eq!(
        ccxt_market_event_log_name(&market),
        worker_scoped_event_log_name("ccxt-market", &market.id)
    );
}

/// 缺身份不再被 `unknown` 兜底名掩盖。
#[test]
fn account_log_writers_fail_closed_without_an_identity() {
    let mut orphan = mk_worker("binance-exec", WorkerRole::Execution, "binance", None);
    orphan.account_id = None;
    let error = binance_event_log_name(&orphan).unwrap_err();
    assert!(
        error.contains("binance-exec") && error.contains("account_id"),
        "缺身份的报错必须点名是哪个 worker 缺什么: {error}"
    );
    assert_eq!(worker_account_event_log(&orphan), None);
    let mut no_venue = mk_worker("paper-exec", WorkerRole::Execution, "paper", None);
    no_venue.venue_id = None;
    assert!(required_account_event_log(&no_venue).is_err());
}

/// paper 模板配置，落到独立的临时 data_dir，规格路径改成绝对路径。
fn paper_identity_runtime(data_dir: &Path) -> RuntimeConfig {
    let deploy = builtin_backtest_example_paths().0;
    let mut config =
        read_runtime_config(&deploy.join("qianxing.runtime.paper-strategy.example.json")).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    for worker in config.workers.iter_mut() {
        if worker.instrument_spec_path.is_some() {
            worker.instrument_spec_path = Some(
                deploy
                    .join("qianxing.binance.spot.spec.json")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    config
}

/// 拆分最严重的那条缝：`paper-submit-order` 播种的初始资金必须落在主链路读的同一本账。
#[test]
fn paper_submit_order_seeds_the_account_log_the_worker_reduces() {
    let root = temp_cli_case_dir("paper-submit-identity");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_path = root.join("runtime.json");
    std::fs::write(
        &config_path,
        paper_identity_runtime(&data_dir).to_json().unwrap(),
    )
    .unwrap();
    let order = mk_order(
        9701,
        &InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        Side::Buy,
        1,
    );
    let command_path = root.join("submit-order.json");
    std::fs::write(
        &command_path,
        serde_json::to_vec(&mk_submit_command(9701, &order, false)).unwrap(),
    )
    .unwrap();

    // 干净目录里没有行情事实，入口按 fail-closed 退出；播种的初始资金已经先落盘。
    assert!(run_paper_submit_order(&config_path, &command_path).is_err());
    assert!(
        !data_dir.join("paper-events.json").is_file(),
        "不得再另起一本没人读的 paper-events 账"
    );
    let pipeline = LiveEventPipeline::open(&data_dir, "paper-main-paper-events", "USDT").unwrap();
    assert_eq!(
        pipeline.ledger().cash_for("main", "USDT"),
        100_000 * SCALE,
        "初始资金必须落在账户身份日志里，主链路才读得到"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 投影源按账户身份去重：同一账户的两种 Venue 拼写只能建一个投影、推一个游标。
#[test]
fn projection_sources_dedupe_by_account_identity() {
    let root = temp_cli_case_dir("paper-submit-dedupe");
    let data_dir = root.join("data");
    let mut config = paper_identity_runtime(&data_dir);
    for worker in config.workers.iter_mut() {
        if worker.id == "paper-spread-recovery" {
            // 把下线中的恢复 worker 拉起来，并用另一种 Venue 拼写声明同一个账户域。
            worker.enabled = true;
            worker.venue_id = Some(" Paper ".into());
        }
    }
    let sources = configured_account_event_logs(&config).unwrap();
    assert_eq!(
        sources
            .iter()
            .map(|(_, _, log_name, _)| log_name.as_str())
            .collect::<Vec<_>>(),
        vec!["paper-main-paper-events"],
        "两种拼写是同一本账，只能建一个投影"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 命名硬切之后，旧名字的账本不再有任何引用者。它留在 data_dir 里就会被当成当前
/// 账本读，所以 doctor 要点名——但不升级为失败：那是运维决定，不是启动前置条件。
#[test]
fn doctor_reports_event_logs_no_identity_owns() {
    let root = temp_cli_case_dir("paper-submit-orphans");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_identity_runtime(&data_dir);
    // 当前身份自己的账本、一本没人认领的旧账、一个不是 EventLog 的相邻文件。
    std::fs::write(data_dir.join("paper-main-paper-events.json"), "{}").unwrap();
    std::fs::write(data_dir.join("paper-events.json"), "{}").unwrap();
    std::fs::write(data_dir.join("control-plane.json"), "{}").unwrap();
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    let report = collect_doctor_report(&config_path).unwrap();
    let warnings = report["warnings"].as_array().unwrap().clone();
    let mentioned = |file: &str| {
        let needle = data_dir.join(file).display().to_string();
        warnings.iter().any(|warning| {
            warning
                .as_str()
                .is_some_and(|message| message.contains(&needle))
        })
    };
    assert!(
        mentioned("paper-events.json"),
        "没人引用的 EventLog 必须被点名: {warnings:?}"
    );
    assert!(
        !mentioned("paper-main-paper-events.json"),
        "在册账本不是孤儿"
    );
    assert!(
        !mentioned("control-plane.json"),
        "EventLog 之外的运行态文件不归这条检查管"
    );
    let checks = report["checks"].as_array().unwrap();
    assert!(
        checks
            .iter()
            .any(|check| check["name"] == "event_logs.orphan" && check["status"] == "warn"),
        "孤儿检查必须落在 warn 而不是 fail: {checks:?}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 命名口径硬切留下的旧账本要靠"改名归档"处置，所以改名不得动到事实：
/// `RunManifest` 的输入摘要吃的是事件序列摘要，一旦日志名混进摘要口径，
/// 每次改名都会让已发布运行产物的指纹对不上，溯源链条当场作废。
#[test]
fn renaming_an_account_log_preserves_fact_and_manifest_digests() {
    let root = temp_cli_case_dir("rename-digest");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let log_name = paper_account_log();
    {
        let mut pipeline = LiveEventPipeline::open(&data_dir, &log_name, "USDT").unwrap();
        for amount in [1_000_i64, 2_000] {
            pipeline
                .ingest(RuntimeEventEnvelope::venue(
                    RuntimeExternalEvent::AccountCashflow {
                        cashflow: AccountCashflow {
                            account_id: "main".into(),
                            venue_id: "paper".into(),
                            currency: "USDT".into(),
                            kind: CashflowKind::Transfer,
                            amount: Money::from_i64(amount),
                            external_id: format!("rename:transfer:{amount}"),
                        },
                    },
                    1,
                    1,
                    1,
                    "rename:transfer",
                ))
                .unwrap();
        }
    }
    let before = LiveEventPipeline::open(&data_dir, &log_name, "USDT")
        .unwrap()
        .log()
        .digest();
    std::fs::rename(
        data_dir.join(format!("{log_name}.json")),
        data_dir.join("legacy-account-events.json"),
    )
    .unwrap();
    let manifest_before = account_log_manifest_digest(before);
    let after = LiveEventPipeline::open(&data_dir, "legacy-account-events", "USDT")
        .unwrap()
        .log()
        .digest();
    assert_eq!(before, after, "改名只换文件名，不得改事件摘要");
    assert_eq!(
        manifest_before,
        account_log_manifest_digest(after),
        "事件摘要不变时，运行 Manifest 指纹必须逐位不变"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 用事件摘要造一份运行 Manifest 指纹，锁住"日志名不在摘要口径里"这件事。
fn account_log_manifest_digest(event_hash: u64) -> u64 {
    let text = format!("{event_hash:016x}");
    let manifest = qx_core::RunManifest {
        run_id: "rename-digest".into(),
        code_commit: "test".into(),
        config_hash: "test".into(),
        data_fingerprint: "test".into(),
        result_hash: "test".into(),
        strategy_version: "test".into(),
        instrument_spec_version: "test".into(),
        model_fingerprint: "test".into(),
        input_event_hash: text.clone(),
        output_event_hash: text,
        runtime_version: "test".into(),
        ..Default::default()
    };
    assert_eq!(manifest.validate(), Ok(()));
    manifest.digest()
}
