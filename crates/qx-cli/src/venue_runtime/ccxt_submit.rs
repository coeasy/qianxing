use crate::ccxt_submit_args::CcxtSubmitArgs;
use crate::*;

/// 校验 CCXT 一次性提交目标 worker：必须是已启用的 CCXT Execution 角色，且带 account_id/venue_id。
///
/// 这里只做拓扑校验——CCXT 配置（exchange_id 与 venue_id 一致、credential_env 形状合法）
/// 由调用方在解析凭据路径之后用 `validate_ccxt_worker_binding` 复核（与 `run_ccxt_worker` 同源闸门）。
fn validate_ccxt_submit_worker(worker: &WorkerConfig) -> Result<(&str, &str), String> {
    if !worker.enabled || worker.role != WorkerRole::Execution {
        return Err(format!(
            "worker {} 必须是已启用的 CCXT Execution 角色才能提交订单",
            worker.id
        ));
    }
    if VenueFamily::parse_option(worker.venue_id.as_deref()) != Some(VenueFamily::Other) {
        return Err(format!(
            "worker {} 不是 CCXT Venue（venue_id 既不能缺省，也不能是 paper 或 binance）",
            worker.id
        ));
    }
    let account_id = worker
        .account_id
        .as_deref()
        .ok_or_else(|| format!("worker {} 缺少 account_id", worker.id))?;
    let venue_id = worker
        .venue_id
        .as_deref()
        .ok_or_else(|| format!("worker {} 缺少 venue_id", worker.id))?;
    Ok((account_id, venue_id))
}

/// 执行一条经过 ControlPlane 审计的 CCXT SubmitOrder 命令（一次性入口）。
///
/// 该入口与 `paper-submit-order` / `binance-submit-order` 同源：配置、命令权限、订单账户/
/// 交易所和 EventLog 先校验；订单事实先写入 `OrderSubmitted`，再由公共 CCXT Worker 归约
/// 成 Accepted/Fill。网络返回未知时 EventLog 保留 Submitted 状态，后续只能通过对账恢复，
/// 绝不自动重试补单。CCXT 凭据只经由环境变量名引用（写在 CCXT 配置 JSON 里），不进命令行、
/// 不进运行时配置、不回显进日志。
pub(crate) fn run_ccxt_submit_order(args: &CcxtSubmitArgs) -> Result<(), String> {
    let runtime_path: &Path = &args.path;
    let worker_id: &str = &args.worker_id;
    let ccxt_config: &Path = &args.ccxt_config;
    let command_path: &Path = &args.command_path;
    let config = read_runtime_config(runtime_path)?;
    let mut worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("找不到 worker: {worker_id}"))?;
    resolve_worker_runtime_paths(&mut worker, runtime_path);
    validate_ccxt_submit_worker(&worker)?;
    // 与 CCXT worker 入口同一条闸门：一次性提交也不能把 A 股段收下就什么都不做（V11 Q65）。
    reject_ashare_rules_on_submit_path(runtime_path, Some(&worker), "ccxt-submit-order")?;
    // 与 `run_ccxt_worker` 同源的 CCXT 配置解析与绑定校验：相对路径按运行时配置目录解析，
    // exchange_id 必须与 worker.venue_id 一致，credential_env 的环境变量名必须非空。
    let ccxt_config_path = resolve_ccxt_config_path(runtime_path, &ccxt_config.to_string_lossy());
    validate_ccxt_worker_binding(&worker, Path::new(&ccxt_config_path))?;
    let settlement_currency = account_worker_settlement_currency(&config, &worker)?;
    let command: ControlCommand = serde_json::from_str(
        &std::fs::read_to_string(command_path)
            .map_err(|error| format!("读取 SubmitOrder 命令失败: {error}"))?,
    )
    .map_err(|error| format!("SubmitOrder 命令 JSON 无效: {error}"))?;
    order_from_submit_command(&command)
        .map_err(|error| format!("SubmitOrder 订单载荷非法: {error:?}"))?;
    // 一次性入口的 Accepted/终态状态机由 qx-service 统一执行；CLI 只装配配置和连接器。
    let pipeline_storage = PipelineStorage::from_config(&config)?;
    let now = runtime_timestamp_ms();
    let store = configured_control_store(&config)?;
    let outcome =
        qx_service::one_shot_submit::run_one_shot_submit(&store, command.clone(), now, || {
            if !ccxt_submit_matches_worker(&command, &worker) {
                // 显式点名 worker 的一次性入口不得把订单写进另一个账户 / 交易所的 EventLog。
                Err("订单 account_id 或 instrument venue 与 worker 拓扑不一致".into())
            } else if let Err(reason) =
                require_worker_risk_spec(&worker, &command, Some(runtime_path))
            {
                Err(reason)
            } else {
                qx_service::ccxt_execution_host::execute_ccxt_one_shot_order(
                    &command,
                    &worker,
                    &pipeline_storage,
                    &settlement_currency,
                    &python_interpreter(),
                    &ccxt_config_path,
                    runtime_path,
                    now,
                )
            }
        })?;
    println!(
        "[CCXT · SubmitOrder] accepted={:?} final={:?} command_id={} result={}",
        outcome.accepted.status,
        outcome.terminal.status,
        command.command_id,
        outcome.terminal.result_code
    );
    outcome.into_result_code().map(|_| ())
}
