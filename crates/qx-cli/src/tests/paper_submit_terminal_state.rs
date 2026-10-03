//! 一次性提交入口的终态收口（V13 第三十一遍 ② #273）。
//!
//! `paper-submit-order` 在把命令写进控制面、入队并领取租约之后，还有一串"读一笔东西"的
//! 步骤：打开账户日志、解析订单载荷、构造风控快照、取最新行情、读成本规则。实测里最常
//! 命中最后两条 —— MarketData worker 还没把行情写进同一本 EventLog。这些位置原本是 `?`，
//! 函数带着一条永远停在 `Accepted` 的命令、一份没人释放的租约直接退出：重投同一条命令
//! 只会撞 `DuplicateRequest`，而文案还在说"等待注入后重试"，运营者据此重试第二次、第三次
//! （实测见 `logs/s769_pass32_btc_paper_submit.txt`）。
//!
//! 两条用例钉同一件事的两个消费者：
//! 1. 一次性入口必须把拒绝写成终态、确认出队，并把真正能走的下一步写进文案。
//! 2. 常驻执行 worker 对同一条命令必须给同一个裁决 —— 它以前会把整个 worker 打停。

use super::*;

/// Paper 运行时夹具：账户日志、执行 worker 与规格都按 BTC 现货那份示例装配，
/// `with_market_quote=false` 时不注入任何行情事实，让提交路径走到 fail-closed 那一格。
struct PaperSubmitCase {
    dir: PathBuf,
    data_dir: PathBuf,
    config_path: PathBuf,
    command_path: PathBuf,
}

fn paper_submit_case(label: &str, client_id: u64, with_market_quote: bool) -> PaperSubmitCase {
    let dir = std::env::temp_dir().join(format!(
        "qianxing-cli-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data_dir = dir.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let template = workspace_root
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    // runtime.json 落在临时目录，相对 spec 会被解析到临时目录之外；固定成工作区内
    // 已验证的 BTCUSDT 现货规格，保证账户级风控这一格是可加载的。
    for worker in config.workers.iter_mut() {
        if worker.instrument_spec_path.is_some() {
            worker.instrument_spec_path = Some(
                workspace_root
                    .join("deploy")
                    .join("qianxing.binance.spot.spec.json")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    let config_path = dir.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let log_name = paper_account_log();
    if with_market_quote {
        let market_ts = runtime_timestamp_ms();
        LiveEventPipeline::open(&data_dir, &log_name, "USDT")
            .unwrap()
            .ingest(RuntimeEventEnvelope::market_quote(
                instrument.clone(),
                QuoteTick::new(
                    market_ts,
                    Price::from_i64(99),
                    Quantity::from_i64(1_000),
                    Price::from_i64(100),
                    Quantity::from_i64(1_000),
                    market_ts,
                ),
                market_ts,
                market_ts,
                "paper-events:terminal-state-test-quote",
            ))
            .unwrap();
    }
    let command = ControlCommand {
        command_id: client_id,
        request_id: format!("paper-submit-{client_id}"),
        operator_id: "paper".into(),
        reason: "terminal state acceptance case".into(),
        kind: CommandKind::SubmitOrder,
        target: client_id.to_string(),
        payload: BTreeMap::from([(
            "order_json".into(),
            serde_json::to_string(&mk_order(client_id, &instrument, Side::Buy, 1)).unwrap(),
        )]),
        permission: Permission::Trading,
        dry_run: false,
    };
    let command_path = dir.join("command.json");
    std::fs::write(&command_path, serde_json::to_string(&command).unwrap()).unwrap();
    PaperSubmitCase {
        dir,
        data_dir,
        config_path,
        command_path,
    }
}

fn queue_of(case: &PaperSubmitCase) -> ControlCommandQueue {
    ControlCommandQueue::new(case.data_dir.join("control-queue"))
}

/// 终态记录里那条命令的最后一格：状态与结果码。
fn terminal_record(case: &PaperSubmitCase, client_id: u64) -> (CommandStatus, String) {
    let state = load_control_state(&case.data_dir).unwrap();
    let record = state
        .audit()
        .iter()
        .rev()
        .find(|record| record.command_id == client_id)
        .expect("提交路径必须留下这条命令的审计记录");
    (record.status, record.result_code.clone())
}

/// 一次性入口：缺行情这条 fail-closed 拒绝要落成终态、确认出队，并把真正的下一步写进文案。
#[test]
fn paper_submit_order_without_market_quote_terminates_and_acks_the_queue() {
    let case = paper_submit_case("v13p31-submit-no-quote", 8001, false);
    let error = run_paper_submit_order(&case.config_path, &case.command_path)
        .expect_err("缺行情的提交必须失败，但不能把命令留在 Accepted");
    // 文档引用的前缀不许腐烂：deploy/README.md 按这段文案向运营者解释 Paper 的 fail-closed。
    assert!(
        error.starts_with("FAIL_CLOSED: Paper SubmitOrder 缺少 BTCUSDT.BINANCE 的最新行情事实"),
        "拒绝文案换了身份，读侧与文档就对不上了: {error}"
    );
    assert!(
        error.contains("command_id") && error.contains("request_id"),
        "命令已经出不了 Accepted，文案必须指出换号重投这条路: {error}"
    );

    let state = load_control_state(&case.data_dir).unwrap();
    assert_eq!(state.audit().len(), 2, "Accepted + 终态，一条都不能少");
    let (status, result_code) = terminal_record(&case, 8001);
    assert_eq!(
        status,
        CommandStatus::Failed,
        "拒绝没有写成终态，这条命令会永远卡在 Accepted"
    );
    assert!(
        result_code.starts_with("FAIL_CLOSED:"),
        "终态码丢了 fail-closed 分类前缀: {result_code}"
    );
    assert!(
        queue_of(&case).pending().unwrap().is_empty(),
        "租约没被确认：命令还挂在队列里，等一个永远不会来的执行者"
    );
    assert!(
        !case
            .data_dir
            .join("control-queue")
            .join("commands")
            .join("8001.lease.json")
            .exists(),
        "领取过的租约文件仍在，等于宣称有一个执行者还在处理它"
    );
    let pipeline = LiveEventPipeline::open(&case.data_dir, paper_account_log(), "USDT").unwrap();
    assert!(
        pipeline.orders().is_empty(),
        "fail-closed 拒绝不得留下任何订单事实"
    );

    // 同一条命令重投仍然只会被幂等闸门挡下 —— 这正是终态+确认必须在这一遍就做完的原因。
    let retry = run_paper_submit_order(&case.config_path, &case.command_path)
        .expect_err("同 request_id 的重投必须被幂等闸门拒绝");
    assert!(
        retry.contains("DuplicateRequest"),
        "重投走的不是幂等闸门，说明控制面记录形态与预期不符: {retry}"
    );
    assert_eq!(
        load_control_state(&case.data_dir).unwrap().audit().len(),
        2,
        "被拒的重投不得再添审计记录"
    );
    let _ = std::fs::remove_dir_all(&case.dir);
}

/// 基线自证：同一份夹具只多注入一条行情事实，提交就必须成交并留下 `Executed` 终态 ——
/// 上一条红的是终态判据，不是"缺行情"这个夹具本身。
#[test]
fn paper_submit_order_with_market_quote_still_reaches_executed() {
    let case = paper_submit_case("v13p31-submit-with-quote", 8002, true);
    run_paper_submit_order(&case.config_path, &case.command_path)
        .expect("有行情的提交必须照常成交，终态闸门不许把正常提交也拦掉");
    assert_eq!(
        terminal_record(&case, 8002).0,
        CommandStatus::Executed,
        "有行情的提交必须照常走到 Executed"
    );
    let pipeline = LiveEventPipeline::open(&case.data_dir, paper_account_log(), "USDT").unwrap();
    assert_eq!(pipeline.orders().len(), 1, "成交事实要留在同一本账户日志");
    assert!(queue_of(&case).pending().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&case.dir);
}

/// 常驻执行 worker 对同一条命令给同一个裁决：拒绝落终态、确认出队，worker 自己照常收尾。
///
/// 修复前这里返回的是 `Err`——「行情还没进来」这一格会把整个执行 worker 打停，
/// 监督器按重启节律反复拉起，命令却始终停在 `Accepted`。
#[test]
fn paper_execution_worker_terminates_a_quote_less_command_and_keeps_running() {
    let case = paper_submit_case("v13p31-worker-no-quote", 8003, false);
    let command: ControlCommand =
        serde_json::from_str(&std::fs::read_to_string(&case.command_path).unwrap()).unwrap();
    let control = ControlStateBackend::Files(JsonStateStore::new(&case.data_dir));
    control
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, 10))
        .unwrap()
        .1
        .unwrap();
    queue_of(&case).enqueue(command.clone(), 10).unwrap();

    run_paper_execution_worker(&case.config_path, "paper-execution", true)
        .expect("缺行情是「还没准备好」，不该把执行 worker 整体打停");

    let (status, result_code) = terminal_record(&case, 8003);
    assert_eq!(
        status,
        CommandStatus::Failed,
        "worker 路径的拒绝也必须落成终态，否则同一条命令会被反复领取"
    );
    assert!(
        result_code.contains("最新行情事实"),
        "拒绝原因必须是缺行情: {result_code}"
    );
    assert!(queue_of(&case).pending().unwrap().is_empty());
    let pipeline = LiveEventPipeline::open(&case.data_dir, paper_account_log(), "USDT").unwrap();
    assert!(
        pipeline
            .orders()
            .iter()
            .all(|order| order.client_id != command.command_id),
        "被拒绝的提交不得留下订单事实"
    );
    let _ = std::fs::remove_dir_all(&case.dir);
}
