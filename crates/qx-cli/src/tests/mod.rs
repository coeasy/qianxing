// qx-cli 的行为用例按主题分文件放在这里（Phase 4o 从单文件 `tests_main.rs` 拆出）。
// 与 `venue_runtime/`、`ledger/` 同一先例：目录模块让每个文件留在 500 行门槛内，
// 于是不必为测试代码单独登记行数预算。共享夹具集中在本文件，主题文件只写用例。
use super::*;
use qx_control::{CommandKind, CommandStatus};
use std::time::{SystemTime, UNIX_EPOCH};

/// Paper smoke fixture 的账户级风控上下文：绑定仓库内已验收的 BTCUSDT
/// 现货规格并提供充足保证金。完整风控矩阵在 `qx-execution` 与 `qx-risk`
/// 的合同测试中覆盖。
pub(crate) fn smoke_paper_risk_context() -> RiskContext {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let spec: TradingInstrumentSpec = serde_json::from_str(
        &std::fs::read_to_string(
            workspace_root
                .join("deploy")
                .join("qianxing.binance.spot.spec.json"),
        )
        .unwrap(),
    )
    .unwrap();
    RiskContext {
        available_margin_raw: Some(1_000_000 * SCALE),
        reference_price: Some(Price::from_i64(100)),
        instrument_spec: Some(spec),
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    }
}

/// 回测示例配置的 deploy 目录、BarFrame 与运行时模板。
pub(crate) fn builtin_backtest_example_paths() -> (PathBuf, PathBuf, PathBuf) {
    let deploy = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    (
        deploy.clone(),
        deploy.join("qianxing.bar-frame.example.json"),
        deploy.join("qianxing.runtime.builtin-strategy.example.json"),
    )
}

/// 为单个用例创建独立临时目录，避免回测产物落在仓库里。
pub(crate) fn temp_cli_case_dir(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// 把配置写入独立的临时 data_dir，避免回测产物落在仓库里。
pub(crate) fn isolated_backtest_runtime(
    deploy: &Path,
    config: &RuntimeConfig,
    label: &str,
) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let mut config = config.clone();
    config.storage.data_dir = root.to_string_lossy().into_owned();
    // 运行时文件搬进临时目录后，相对 deploy 的输入路径要固定为绝对路径。
    config.strategy.bars_snapshot_path = Some(
        deploy
            .join("qianxing.bar-frame.example.json")
            .to_string_lossy()
            .into_owned(),
    );
    config.strategy.dataset_bundle_path = Some(
        deploy
            .join("qianxing.dataset-bundle.bar-frame.example.json")
            .to_string_lossy()
            .into_owned(),
    );
    let runtime_path = root.join("runtime.json");
    std::fs::write(
        &runtime_path,
        serde_json::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
    (root, runtime_path)
}

/// 构造一份只做字段填装的 worker 配置，避免每个风控用例重复 14 个字段。
pub(crate) fn mk_worker(
    id: &str,
    role: WorkerRole,
    venue: &str,
    instrument_spec_path: Option<&str>,
) -> WorkerConfig {
    WorkerConfig {
        id: id.into(),
        role,
        enabled: true,
        account_id: Some("main".into()),
        venue_id: Some(venue.into()),
        endpoint: None,
        symbols: Vec::new(),
        settlement_currency: None,
        credential_env: None,
        credential_files: None,
        instrument_spec_path: instrument_spec_path.map(str::to_string),
        paper_initial_cash_raw: None,
        max_order_notional_raw: None,
        max_position_notional_raw: None,
    }
}

/// 仓库内已验收的 BTCUSDT 现货规格绝对路径，供风控配置用例直接引用。
pub(crate) fn workspace_binance_spot_spec() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.binance.spot.spec.json")
}

/// 装配一条 SubmitOrder 控制命令；`dry_run=false` 时进入真实副作用分支。
pub(crate) fn mk_submit_command(command_id: u64, order: &Order, dry_run: bool) -> ControlCommand {
    ControlCommand {
        command_id,
        request_id: format!("submit-{command_id}"),
        operator_id: "ops".into(),
        reason: "risk boundary integration".into(),
        kind: CommandKind::SubmitOrder,
        target: command_id.to_string(),
        payload: BTreeMap::from([("order_json".into(), serde_json::to_string(order).unwrap())]),
        permission: Permission::Trading,
        dry_run,
    }
}

/// 子进程型用例共用的被测 binary 与其新鲜度护栏（原本只在 `cli_surface.rs` 内，
/// V11 Q0b 的旗标用例同样要跑真 binary，于是按"共享夹具进本文件"的约定上移）。
const QX_CLI_SURFACE_SOURCES: [&str; 4] = [
    "src/cli.rs",
    "src/cli_help.rs",
    "src/config_commands.rs",
    "src/backtests/mod.rs",
];

/// 被测 binary 是否不早于被测源码：`cargo test --bin` 只编译测试壳、不会重链
/// `target/debug/qx-cli.exe`，放任过期 binary 会让子进程断言对着旧行为"绿"。
fn assert_binary_fresh(binary: &Path) {
    let built = binary
        .metadata()
        .and_then(|m| m.modified())
        .expect("读取被测 binary 修改时间失败");
    for source in QX_CLI_SURFACE_SOURCES {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(source);
        let edited = path
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or_else(|_| panic!("找不到被测源码 {}", path.display()));
        assert!(
            built >= edited,
            "被测 binary {} 比 {} 旧，请先 cargo build -p qx-cli 再跑本用例",
            binary.display(),
            path.display()
        );
    }
}

fn qx_cli_binary() -> PathBuf {
    if let Some(path) = option_env!("CARGO_BIN_EXE_qx-cli") {
        let binary = PathBuf::from(path);
        assert_binary_fresh(&binary);
        return binary;
    }
    let profile_dir = std::env::current_exe()
        .expect("读取测试可执行文件路径失败")
        .parent()
        .and_then(|deps| deps.parent())
        .expect("测试可执行文件应位于 target/<profile>/deps")
        .to_path_buf();
    let binary = profile_dir.join(if cfg!(windows) {
        "qx-cli.exe"
    } else {
        "qx-cli"
    });
    assert!(binary.is_file(), "未找到被测 binary {}", binary.display());
    binary
}

mod backtest_entries;
mod backtest_risk_provenance;
mod cli_surface;
mod e2e_and_python_contract;
mod execution_and_multi_leg;
mod live_submit_fail_closed;
mod paper_and_strategy_worker;
mod paper_bridge_and_bundles;
mod worker_observability;
