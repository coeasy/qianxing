//! 一份 runtime 只能有一个账户本金口径（V12 R3 / §4.4）。
//!
//! 回测读 `strategy.initial_cash_raw`，Paper 读 `worker.paper_initial_cash_raw`，两侧过去互不
//! 知情：同一份配置写 100,000 与 200,000 也能启动，回测按前者记分母、Paper 按后者入账初始资金，
//! 使用者却以为"我声明了一份本金"。这里的用例把这条拆口钉回去：不等即拒、等值要说出来。

use super::*;

const PAPER_RUNTIME: &str = "qianxing.runtime.paper-strategy.example.json";
/// 与提交入口用例同一份夹具：这份配置里有一个真正会入账的 Paper 执行 worker。
const PAPER_EXECUTION_WORKER: &str = "paper-execution";
/// 本金是 1e-9 定点数，夹具里的写法与真实配置同尺度。
const PRINCIPAL_RAW: i128 = 250_000 * 1_000_000_000;
const OTHER_PRINCIPAL_RAW: i128 = 900_000 * 1_000_000_000;

fn deploy(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("仓库根目录")
        .join("deploy")
        .join(name)
}

/// 夹具配置：data_dir 指向独立临时目录，本金声明由 `edit` 逐用例改动，其余口径与基线逐字相同。
fn principal_runtime(
    label: &str,
    edit: impl FnOnce(&mut RuntimeConfig),
) -> (PathBuf, PathBuf, RuntimeConfig) {
    let root = temp_cli_case_dir(label);
    let mut config = read_runtime_config(&deploy(PAPER_RUNTIME)).unwrap();
    config.storage.data_dir = root.join("data").to_string_lossy().into_owned();
    for worker in config.workers.iter_mut() {
        if worker.instrument_spec_path.is_some() {
            worker.instrument_spec_path = Some(
                deploy("qianxing.binance.spot.spec.json")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        // 示例夹具自带一份 paper 初始资金：不先摘掉，"只声明一处"的用例就会撞在自己
        // 没写的那一格上，测出来的冲突与用例名字无关。
        worker.paper_initial_cash_raw = None;
    }
    edit(&mut config);
    let path = root.join("runtime.json");
    std::fs::write(&path, config.to_json().unwrap()).unwrap();
    (root, path, config)
}

fn set_strategy_cash(config: &mut RuntimeConfig, raw: i128) {
    config.strategy.initial_cash_raw = Some(raw);
}

fn set_worker_cash(config: &mut RuntimeConfig, id: &str, raw: i128) {
    let worker = config
        .workers
        .iter_mut()
        .find(|worker| worker.id == id)
        .unwrap_or_else(|| panic!("夹具配置里没有 worker {id}"));
    worker.paper_initial_cash_raw = Some(raw);
}

/// 两处声明同一个数：回测侧的来源要说出"这一格被 paper 侧确认过"，Paper 侧要把它印出来。
/// 等值不等于无话可说 —— 读者若看不出两处共用一个本金，就会在改其中一处时以为只改了一件事。
#[test]
fn the_same_principal_declared_twice_is_reported_as_one_agreeing_number() {
    let (root, _, config) = principal_runtime("v12r3-agreeing", |config| {
        set_strategy_cash(config, PRINCIPAL_RAW);
        set_worker_cash(config, PAPER_EXECUTION_WORKER, PRINCIPAL_RAW);
    });
    let base = account_base_from_config(&config).unwrap();
    assert_eq!(base.cash.raw(), PRINCIPAL_RAW);
    assert_eq!(
        base.source,
        BACKTEST_ACCOUNT_BASE_BOTH_DECLARED_SOURCE,
        "两处声明一致必须与「只有回测侧声明」分得开: {:?}",
        backtest_account_base_note(base)
    );
    let note = account_principal_note(&config).expect("两处一致时 Paper 侧必须说出来");
    assert!(
        note.contains("两处声明一致")
            && note.contains(&format!("initial_cash_raw={PRINCIPAL_RAW}"))
            && note.contains("strategy.initial_cash_raw")
            && note.contains(&format!("worker[{PAPER_EXECUTION_WORKER}]")),
        "说明要点出生效的数与两处名字: {note}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 只有一处声明时不得冒充"两处一致"：那是一句没有依据的确认。
#[test]
fn a_lone_backtest_declaration_is_not_reported_as_agreeing_with_anything() {
    let (root, _, config) = principal_runtime("v12r3-lone-strategy", |config| {
        set_strategy_cash(config, PRINCIPAL_RAW);
    });
    let base = account_base_from_config(&config).unwrap();
    assert_eq!(base.source, BACKTEST_ACCOUNT_BASE_CONFIG_SOURCE);
    assert!(
        account_principal_note(&config).is_none(),
        "没有第二处声明就没有「一致」可说"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 判据的方向只有回测侧 → paper 侧：两个 Paper 账户各自定资是不同的账户，不是两份口径。
/// 把这条写反会让多账户拓扑连启动都做不到 —— 那等于用一条正确的规则制造新的假阴性。
#[test]
fn two_paper_accounts_may_be_funded_differently_without_a_backtest_declaration() {
    let (root, _, mut config) = principal_runtime("v12r3-two-accounts", |config| {
        set_worker_cash(config, PAPER_EXECUTION_WORKER, PRINCIPAL_RAW);
    });
    let mut second = mk_worker("paper-execution-2", WorkerRole::Execution, "paper", None);
    second.account_id = Some("sub".into());
    second.paper_initial_cash_raw = Some(OTHER_PRINCIPAL_RAW);
    config.workers.push(second);
    reject_split_account_principal(&config)
        .expect("没有格子声称全局本金时，两个账户各自定资是合法的");
    let base = account_base_from_config(&config).unwrap();
    assert_eq!(
        base.source, BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE,
        "回测侧没声明就照默认走，来源仍要写成 builtin-default"
    );
    assert!(account_principal_note(&config).is_none());
    let _ = std::fs::remove_dir_all(root);
}

/// 不等即拒，且两个入口都拒：回测侧算不出唯一分母，Paper 侧也不能按自己那一格先入账。
#[test]
fn two_different_principals_in_one_runtime_are_rejected_everywhere_they_are_read() {
    let (root, path, config) = principal_runtime("v12r3-split", |config| {
        set_strategy_cash(config, PRINCIPAL_RAW);
        set_worker_cash(config, PAPER_EXECUTION_WORKER, OTHER_PRINCIPAL_RAW);
    });
    let error = reject_split_account_principal(&config).expect_err("两份本金不等必须当场拒");
    assert!(
        error.contains("两份互不相等的账户本金")
            && error.contains(&format!("strategy.initial_cash_raw={PRINCIPAL_RAW}"))
            && error.contains(&format!(
                "worker[{PAPER_EXECUTION_WORKER}]={OTHER_PRINCIPAL_RAW}"
            )),
        "报错要并列点出两处名字与各自的数: {error}"
    );
    assert!(
        account_base_from_config(&config).is_err() && configured_account_base(Some(&path)).is_err(),
        "回测侧的两种读法都必须认这条判据"
    );
    // 命令行入口：闸门要排在任何副作用之前。命令文件故意指向不存在的路径，
    // 若走到读命令那一步，报错就会是"命令失败"而不是本金冲突。
    let command = root.join("command.never-written.json");
    let error = run_paper_submit_order(&path, &command)
        .expect_err("Paper 提交入口不能按自己那一格先把钱入账");
    assert!(
        error.contains("两份互不相等的账户本金") && !error.contains("命令失败"),
        "本金闸门必须排在读命令与入账之前: {error}"
    );
    let error = run_paper_execution_worker(&path, PAPER_EXECUTION_WORKER, true)
        .expect_err("Paper worker 同样不能带着两份口径启动");
    assert!(
        error.contains("两份互不相等的账户本金"),
        "worker 入口的拒绝理由不完整: {error}"
    );
    assert!(
        !root.join("data").exists(),
        "被拒的入口不得留下控制面/账本目录这种半截副作用"
    );
    let _ = std::fs::remove_dir_all(root);

    // 基线：同一份夹具只把第二格改成同一个数，两个入口都要照常走得通。
    let (baseline_root, baseline_path, baseline) =
        principal_runtime("v12r3-split-baseline", |config| {
            set_strategy_cash(config, PRINCIPAL_RAW);
            set_worker_cash(config, PAPER_EXECUTION_WORKER, PRINCIPAL_RAW);
        });
    reject_split_account_principal(&baseline).expect("等值声明不是冲突");
    assert!(account_principal_note(&baseline).is_some());
    let error = run_paper_submit_order(&baseline_path, &baseline_root.join("no-command.json"))
        .expect_err("基线仍会走到读命令那一步");
    assert!(
        !error.contains("两份互不相等的账户本金"),
        "摘掉冲突后不得再被这条闸门挡住: {error}"
    );
    let _ = std::fs::remove_dir_all(baseline_root);
}
