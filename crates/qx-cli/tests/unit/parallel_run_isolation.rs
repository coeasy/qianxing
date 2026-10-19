//! 多账户并行运行的隔离验收（V13 R28）。
//!
//! 「多账户 / 多 Venue / 多运行模式同时跑」的前提是一条隔离不变量：同一份 runtime 里
//! 两个账户各跑各的执行 worker 时，事实只落进自己那本账户 EventLog，命令也只被声明了
//! 同一账户的 worker 消费。此前这条不变量只有命名层用例
//! （`account_event_log_identity.rs` 证明名字拼得对）与读侧去重用例
//! （`projection_sources_dedupe_by_account_identity` 证明同一账户不分裂），没有一条用例
//! 真的把两个账户的 worker 各跑一遍、再翻开两本账看有没有串账——**名字拼得对不等于事实不串**。
//!
//! 四条用例各钉一段：
//! ① 两个账户在同一 Venue 上各跑一次真实执行 worker，本金各归各的账（写侧）；
//! ② 一条声明给 A 账户的 SubmitOrder，不得被 B 账户的 worker 消费掉（命令路由）；
//! ③ 两个账户各得一个隔离投影源，读侧不得只留一份（读侧）；
//! ④ 两个账户的执行 worker **同时**跑（屏障强制重叠），隔离在真并发下也成立。

use super::*;

/// 第二个账户的账户名与它的 worker id。日志名一律经 [`account_event_log_name`] 派生，
/// 用例里不抄字面量：测试里的第二份命名就是生产里第二份命名的温床。
const SECOND_ACCOUNT: &str = "account-b";
const SECOND_WORKER_ID: &str = "paper-execution-b";

/// 主账户与第二账户各自的初始本金（raw 定点整数，`SCALE` = 1e9）。
/// 两个数刻意不同：若某条路径把两本账串起来，读数会落在错误的一侧而当场可见。
const MAIN_PRINCIPAL_RAW: i128 = 100_000 * SCALE;
const SECOND_PRINCIPAL_RAW: i128 = 250_000 * SCALE;

/// 账户在 `paper` Venue 上的日志名（唯一构造点派生）。
fn account_log(account: &str) -> String {
    account_event_log_name(account, "paper").expect("paper 账户身份合法")
}

/// 两个账户各一个 Paper 执行 worker，落在独立临时 data_dir；规格路径绝对化。
///
/// 第二账户用 `mk_worker` 装配后只改账户名与本金——不复制整段 `WorkerConfig` 字面量，
/// 这样 `WorkerConfig` 每加一格字段，这份夹具自动跟上，不会悄悄少填。
fn two_account_runtime(data_dir: &Path) -> RuntimeConfig {
    let mut config = paper_runtime_config(data_dir);
    let spec = workspace_binance_spot_spec().to_string_lossy().into_owned();
    let main = config
        .workers
        .iter_mut()
        .find(|worker| worker.id == "paper-execution")
        .expect("paper 模板必须有 paper-execution worker");
    main.paper_initial_cash_raw = Some(MAIN_PRINCIPAL_RAW);
    let mut second = mk_worker(
        SECOND_WORKER_ID,
        WorkerRole::Execution,
        "paper",
        Some(spec.as_str()),
    );
    second.account_id = Some(SECOND_ACCOUNT.into());
    second.paper_initial_cash_raw = Some(SECOND_PRINCIPAL_RAW);
    config.workers.push(second);
    config
}

/// 把两个账户的配置落到独立临时目录，返回 (root, data_dir, config_path)。
fn two_account_fixture(label: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = temp_cli_case_dir(label);
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config_path = root.join("runtime.json");
    std::fs::write(
        &config_path,
        two_account_runtime(&data_dir).to_json().unwrap(),
    )
    .unwrap();
    (root, data_dir, config_path)
}

/// ① 写侧：两个账户在同一 Venue 上各跑一次真实执行 worker，本金各归各的账。
#[test]
fn two_accounts_on_one_venue_keep_separate_books() {
    let (root, data_dir, config_path) = two_account_fixture("parallel-two-accounts");

    // 两条真实生产入口：不是在本用例里重造一份入账逻辑，而是走 `run_paper_execution_worker`
    // 本身，账户身份由各自的 worker 声明决定。
    run_paper_execution_worker(&config_path, "paper-execution", true).unwrap();
    run_paper_execution_worker(&config_path, SECOND_WORKER_ID, true).unwrap();

    let main_log = account_log("main");
    let second_log = account_log(SECOND_ACCOUNT);
    assert_ne!(
        main_log, second_log,
        "两个账户必须各有一本账，否则第二本覆盖第一本"
    );

    let main = LiveEventPipeline::open(&data_dir, &main_log, "USDT").unwrap();
    assert_eq!(
        main.ledger().cash_for("main", "USDT"),
        MAIN_PRINCIPAL_RAW,
        "主账户本金必须落在主账户账上"
    );
    assert_eq!(
        main.ledger().cash_for(SECOND_ACCOUNT, "USDT"),
        0,
        "第二账户的本金不得串进主账户账"
    );

    let second = LiveEventPipeline::open(&data_dir, &second_log, "USDT").unwrap();
    assert_eq!(
        second.ledger().cash_for(SECOND_ACCOUNT, "USDT"),
        SECOND_PRINCIPAL_RAW,
        "第二账户本金必须落在第二账户账上"
    );
    assert_eq!(
        second.ledger().cash_for("main", "USDT"),
        0,
        "主账户本金不得串进第二账户账"
    );

    assert!(
        !data_dir.join("paper-events.json").is_file(),
        "不得另起一本没人读的合账：两本账之外任何一本都是无主事实"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// ② 命令路由：一条声明给主账户的 SubmitOrder 不得被第二账户的 worker 消费掉。
///
/// 这是多账户并行的**关键不对称**：控制面与命令队列是所有 worker 共享的，只有
/// `paper_submit_matches_worker` 那一道账户过滤把命令分派给正确的账户。若这道过滤
/// 被抹掉，第二账户的 worker 会先领走主账户的命令并把它记成终态——主账户此后永远
/// 拿不到自己的订单，且账面上看不出任何异常。
#[test]
fn a_submit_command_scoped_to_one_account_is_invisible_to_the_other_account_worker() {
    let (root, data_dir, config_path) = two_account_fixture("parallel-command-scope");

    // 主账户的行情先落进主账户账，撮合才走得通；第二账户的 worker 不该碰它。
    let main_log = account_log("main");
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    {
        let mut pipeline = LiveEventPipeline::open(&data_dir, &main_log, "USDT").unwrap();
        let ts = runtime_timestamp_ms();
        pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                instrument.clone(),
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
                "parallel:main:quote",
            ))
            .unwrap();
    }

    // 一条声明给主账户的 SubmitOrder：进入共享控制面与共享命令队列。
    let order = mk_order(9801, &instrument, Side::Buy, 1);
    assert_eq!(order.account_id, "main", "夹具订单必须声明给主账户");
    let command = mk_submit_command(9801, &order, false);
    let control = ControlStateBackend::Files(JsonStateStore::new(&data_dir));
    control
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, 10))
        .unwrap()
        .1
        .unwrap();
    ControlCommandQueue::new(data_dir.join("control-queue"))
        .enqueue(command.clone(), 10)
        .unwrap();

    // 第二账户的 worker 先跑：它声明的是 account-b，这条主账户命令必须被跳过。
    run_paper_execution_worker(&config_path, SECOND_WORKER_ID, true).unwrap();
    let second = LiveEventPipeline::open(&data_dir, account_log(SECOND_ACCOUNT), "USDT").unwrap();
    assert!(
        second.orders().is_empty(),
        "主账户的命令不得被第二账户的 worker 撮合成事实"
    );

    // 主账户的 worker 随后跑：命令仍可被消费。若第二账户 worker 曾领走并确认，
    // 队列里就再也取不到它，这一条断言会以"订单数为 0"当场红。
    run_paper_execution_worker(&config_path, "paper-execution", true).unwrap();
    let main = LiveEventPipeline::open(&data_dir, &main_log, "USDT").unwrap();
    assert_eq!(
        main.orders().len(),
        1,
        "声明给主账户的命令必须由主账户 worker 消费"
    );
    assert_eq!(main.orders()[0].account_id, "main");
    let _ = std::fs::remove_dir_all(root);
}

/// ③ 读侧：两个账户各得一个隔离投影源，且各带自己的 `(account_id, venue_id)`。
///
/// API 投影桥按 `configured_account_event_logs` 逐账户建读模型。若它把两个账户
/// 折成一份（或漏掉一个），多账户部署下控制台只会看到其中一个账户的账。
#[test]
fn each_configured_account_gets_its_own_projection_source() {
    let (root, data_dir, _) = two_account_fixture("parallel-projections");
    let config = two_account_runtime(&data_dir);
    let sources = configured_account_event_logs(&config).unwrap();
    let mut seen = sources
        .iter()
        .map(|(account_id, venue_id, log_name, currency)| {
            (
                account_id.as_str(),
                venue_id.as_str(),
                log_name.as_str(),
                currency.as_str(),
            )
        })
        .collect::<Vec<_>>();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            (
                SECOND_ACCOUNT,
                "paper",
                account_log(SECOND_ACCOUNT).as_str(),
                "USDT"
            ),
            ("main", "paper", account_log("main").as_str(), "USDT"),
        ],
        "两个账户各得一个投影源，身份与日志名必须一一对应"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// ④ 并发证据：两个账户的执行 worker **同时**跑（各占一条线程，用屏障强制重叠）。
///
/// 前三条给的是隔离的**语义**证据（顺序调用下成立）。`supervise` 里多个 worker 是同时
/// 活的，所以隔离必须在**真并发**下也成立：两条线程各走一次真实生产入口、写同一个
/// data_dir（控制面与命令队列都是共享的），而事实仍只落进自己那本账户账。
#[test]
fn two_accounts_running_concurrently_keep_separate_books() {
    let (root, data_dir, config_path) = two_account_fixture("parallel-concurrent");
    // 屏障让两条线程真正重叠，而不是被调度器排成一前一后。
    let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
    let first = {
        let path = config_path.clone();
        let gate = std::sync::Arc::clone(&gate);
        thread::spawn(move || {
            gate.wait();
            run_paper_execution_worker(&path, "paper-execution", true)
        })
    };
    let second = {
        let path = config_path.clone();
        let gate = std::sync::Arc::clone(&gate);
        thread::spawn(move || {
            gate.wait();
            run_paper_execution_worker(&path, SECOND_WORKER_ID, true)
        })
    };
    first.join().unwrap().unwrap();
    second.join().unwrap().unwrap();

    let main = LiveEventPipeline::open(&data_dir, account_log("main"), "USDT").unwrap();
    assert_eq!(
        main.ledger().cash_for("main", "USDT"),
        MAIN_PRINCIPAL_RAW,
        "并发下主账户本金必须仍落在主账户账上"
    );
    assert_eq!(
        main.ledger().cash_for(SECOND_ACCOUNT, "USDT"),
        0,
        "并发下第二账户的本金不得串进主账户账"
    );
    let second = LiveEventPipeline::open(&data_dir, account_log(SECOND_ACCOUNT), "USDT").unwrap();
    assert_eq!(
        second.ledger().cash_for(SECOND_ACCOUNT, "USDT"),
        SECOND_PRINCIPAL_RAW,
        "并发下第二账户本金必须仍落在第二账户账上"
    );
    assert_eq!(
        second.ledger().cash_for("main", "USDT"),
        0,
        "并发下主账户本金不得串进第二账户账"
    );
    let _ = std::fs::remove_dir_all(root);
}
