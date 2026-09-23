//! Paper/Live 提交入口对 `strategy.ashare_rules_path` 的态度（V11 Q65）：配了又没有闸门时，
//! 必须在任何副作用之前当场拒，而不是收下配置再按"无交易制度"下单。
//!
//! 与 Q61 同族的另一半：Q61 让 A 股段在四条 Bar 回测链上要么生效要么拒，本文件管的是**提交侧**。
//! 用例都走真实入口函数，差异只在配置里有没有那一个键 —— 证明被拒的原因是键，不是夹具本身坏了。

use super::*;

const PAPER_RUNTIME: &str = "qianxing.runtime.paper-strategy.example.json";
const ASHARE_RULES: &str = "qianxing.ashare.rules.json";

fn deploy(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("仓库根目录")
        .join("deploy")
        .join(name)
}

/// Paper 示例配置 + 独立临时 data_dir；`with_ashare_rules` 决定是否写上那三个键。
fn paper_runtime(label: &str, with_ashare_rules: bool) -> (PathBuf, PathBuf) {
    let root = temp_cli_case_dir(label);
    let mut config = read_runtime_config(&deploy(PAPER_RUNTIME)).unwrap();
    config.storage.data_dir = root.join("data").to_string_lossy().into_owned();
    // 规格文件按运行时配置目录解析，配置搬进临时目录后要固定成绝对路径。
    for worker in config.workers.iter_mut() {
        if worker.instrument_spec_path.is_some() {
            worker.instrument_spec_path = Some(
                deploy("qianxing.binance.spot.spec.json")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    if with_ashare_rules {
        let strategy = &mut config.strategy;
        strategy.ashare_rules_path = Some(deploy(ASHARE_RULES).to_string_lossy().into_owned());
        strategy.ashare_actions_path = Some(
            deploy("qianxing.ashare.actions.example.json")
                .to_string_lossy()
                .into_owned(),
        );
        strategy.ashare_calendar_path = Some(
            deploy("qianxing.ashare.calendar.example.json")
                .to_string_lossy()
                .into_owned(),
        );
    }
    let path = root.join("runtime.json");
    std::fs::write(&path, config.to_json().unwrap()).unwrap();
    (root, path)
}

/// 命令文件故意指向不存在的路径：闸门若排在读命令之后，报错就会是"读取 SubmitOrder 命令失败"。
fn missing_command(root: &Path) -> PathBuf {
    root.join("command.never-written.json")
}

#[test]
fn paper_submit_entry_refuses_the_ashare_section_before_touching_anything() {
    let (root, path) = paper_runtime("q65-paper-submit", true);
    let error = run_paper_submit_order(&path, &missing_command(&root))
        .expect_err("配了 A 股段又执行不了时必须失败，而不是按无制度下单");
    assert!(
        error.contains("strategy.ashare_rules_path") && error.contains("paper-submit-order"),
        "报错要点名是哪个入口拒的哪一段: {error}"
    );
    assert!(
        error.contains("T+1") && error.contains("strategy backtest"),
        "要给出缺的能力与可用的替代入口: {error}"
    );
    assert!(
        !error.contains("命令失败"),
        "闸门必须排在读命令之前，否则拒单发生在副作用之后: {error}"
    );
    assert!(
        !root.join("data").exists(),
        "被拒的入口不能留下控制面/账本目录这种半截副作用"
    );
    // 同一份夹具、只摘掉那三个键：必须换成"读命令失败"这一条路径，证明红的是键不是夹具。
    let (baseline_root, baseline_path) = paper_runtime("q65-paper-submit-baseline", false);
    let error = run_paper_submit_order(&baseline_path, &missing_command(&baseline_root))
        .expect_err("命令文件本就不存在");
    assert!(
        !error.contains("strategy.ashare_rules_path") && error.contains("命令失败"),
        "缺 A 股段时不该被这条闸门挡住: {error}"
    );
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(baseline_root);
}

#[test]
fn paper_worker_entry_refuses_to_start_an_execution_worker_with_ashare_rules() {
    let (root, path) = paper_runtime("q65-paper-worker", true);
    let error = run_paper_execution_worker(&path, "paper-execution", true)
        .expect_err("Paper 执行 worker 不能收下 A 股段再静默按无制度跑");
    assert!(
        error.contains("strategy.ashare_rules_path") && error.contains("paper-worker"),
        "worker 入口的拒绝文案要点名自己: {error}"
    );
    let _ = std::fs::remove_dir_all(root);
    // 基线：同一 worker 在没有 A 股段时能跑完一次扫描。
    let (baseline_root, baseline_path) = paper_runtime("q65-paper-worker-baseline", false);
    run_paper_execution_worker(&baseline_path, "paper-execution", true)
        .expect("没有 A 股段时 Paper 执行 worker 必须照常启动");
    let _ = std::fs::remove_dir_all(baseline_root);
}

#[test]
fn binance_submit_entry_refuses_the_ashare_section_before_reading_the_command() {
    let (root, path) = paper_runtime("q65-binance-submit", true);
    let spec = deploy("qianxing.binance.spot.spec.json")
        .to_string_lossy()
        .into_owned();
    let mut worker = mk_worker(
        "binance-exec",
        WorkerRole::Execution,
        "binance-testnet",
        Some(&spec),
    );
    // 只要求"配了凭据来源"，闸门排在真正读环境变量之前，所以这里放的是变量名而不是密钥。
    worker.credential_env = Some(qx_runtime::CredentialEnv {
        api_key: "QX_TEST_BINANCE_KEY".into(),
        secret: "QX_TEST_BINANCE_SECRET".into(),
    });
    let mut config = read_runtime_config(&path).unwrap();
    config.workers.push(worker);
    std::fs::write(&path, config.to_json().unwrap()).unwrap();
    let error = run_binance_submit_order(&path, "binance-exec", &missing_command(&root))
        .expect_err("实盘提交入口同样不能收下 A 股段再当没事发生");
    assert!(
        error.contains("strategy.ashare_rules_path")
            && error.contains("binance-submit-order")
            && !error.contains("命令失败"),
        "要由 A 股闸门先拒掉，而不是走到读命令那一步: {error}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 第三条提交入口：`ccxt-worker` 的闸门要排在解析 CCXT 配置文件之前 —— 那条路径会去起 Python
/// 进程，本机没有凭据也不该先跑到那里。
#[test]
fn ccxt_worker_entry_refuses_the_ashare_section_before_touching_the_ccxt_config() {
    let (root, path) = paper_runtime("q65-ccxt-worker", true);
    let spec = deploy("qianxing.binance.spot.spec.json")
        .to_string_lossy()
        .into_owned();
    let mut config = read_runtime_config(&path).unwrap();
    config.workers.push(mk_worker(
        "ccxt-exec",
        WorkerRole::Execution,
        "okx",
        Some(&spec),
    ));
    std::fs::write(&path, config.to_json().unwrap()).unwrap();
    let error = run_ccxt_worker(
        &path,
        "ccxt-exec",
        &root.join("ccxt-config.never-written.json"),
        true,
    )
    .expect_err("CCXT 执行 worker 也不能收下 A 股段再静默按无制度跑");
    assert!(
        error.contains("strategy.ashare_rules_path") && error.contains("ccxt-worker"),
        "要由 A 股闸门先拒掉: {error}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 闸门读的是哪一块决定它是不是摆设：`config validate` 与 `strategy backtest` 都认
/// `strategies[]`，只看 legacy `strategy` 就等于留一条"把 A 股段写进列表"的绕过路径。
#[test]
fn the_ashare_gate_also_reads_the_strategies_list() {
    let worker_id = "strategy-paper";
    // 多策略条目必须绑定一个同 id 的启用 Strategy worker，否则先撞拓扑校验、到不了闸门。
    let seeded = |label: &str, with_ashare_rules: bool| -> (PathBuf, PathBuf) {
        let (root, path) = paper_runtime(label, false);
        let mut config = read_runtime_config(&path).unwrap();
        let mut entry = config.strategy.clone();
        entry.id = Some(worker_id.into());
        entry.target_snapshot_path = Some(
            deploy("qianxing.strategy-target.paper.json")
                .to_string_lossy()
                .into_owned(),
        );
        if with_ashare_rules {
            entry.ashare_rules_path = Some(deploy(ASHARE_RULES).to_string_lossy().into_owned());
        }
        config.strategies.push(entry);
        std::fs::write(&path, config.to_json().unwrap()).unwrap();
        (root, path)
    };
    let worker = mk_worker("scoped", WorkerRole::Execution, "paper", None);
    // 反向对照：同一份拓扑、只是没写 A 股段时必须过闸门，证明红的是那个键。
    let (clean_root, clean_path) = seeded("q65-strategies-list-clean", false);
    reject_ashare_rules_on_submit_path(&clean_path, Some(&worker), "paper-submit-order")
        .expect("列表里没有 A 股段就不该被拒");
    let (root, path) = seeded("q65-strategies-list", true);
    let error = reject_ashare_rules_on_submit_path(&path, Some(&worker), "paper-submit-order")
        .expect_err("列表里的 A 股段同样必须当场拒");
    assert!(
        error.contains(&format!("strategy[{worker_id}].ashare_rules_path")),
        "拒绝必须点名是哪一条策略声明的，而不是含糊说 strategy: {error}"
    );
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(clean_root);
}

/// 闸门只管会提交新订单的角色。把行情、对账与策略 worker 一起拒，会让"研究用配置"连启动都做不到 ——
/// 那等于用一条正确的规则去制造一个新的假阴性。
#[test]
fn only_roles_that_submit_orders_meet_the_ashare_gate() {
    let (root, path) = paper_runtime("q65-role-scope", true);
    for role in [WorkerRole::Execution, WorkerRole::SpreadRecovery] {
        let worker = mk_worker("scoped", role, "paper", None);
        let error = reject_ashare_rules_on_submit_path(&path, Some(&worker), "entry")
            .expect_err("提交类角色必须被闸门挡住");
        assert!(
            error.contains("strategy.ashare_rules_path"),
            "{role:?} 侧的拒绝文案不完整: {error}"
        );
    }
    for role in [
        WorkerRole::Strategy,
        WorkerRole::MarketData,
        WorkerRole::UserStream,
        WorkerRole::Reconciler,
        WorkerRole::Scheduler,
        WorkerRole::Api,
    ] {
        let worker = mk_worker("scoped", role, "paper", None);
        reject_ashare_rules_on_submit_path(&path, Some(&worker), "entry")
            .unwrap_or_else(|error| panic!("{role:?} 不提交新订单，不该被 A 股闸门拒绝: {error}"));
    }
    let _ = std::fs::remove_dir_all(root);
}
