//! 本机进程托管：拓扑到子进程参数的映射、监督循环与恢复子进程。

use super::*;

#[cfg(test)]
pub(crate) fn managed_worker_args(
    config: &RuntimeConfig,
    config_path: &Path,
    allow_unmanaged_roles: bool,
) -> Result<Vec<(String, Vec<String>)>, String> {
    Ok(
        qx_orchestrator::plan_workers(config, config_path, allow_unmanaged_roles)?
            .into_iter()
            .map(|launch| (launch.worker_id, launch.args))
            .collect(),
    )
}

/// 跨平台进程托管入口。它只做拓扑校验、日志隔离和 fail-fast 生命周期管理，
/// 不替代 worker 的租约、幂等和 EventLog 恢复语义；任一子进程异常退出时会
/// 停止其余进程，避免 API/策略仍运行而执行器已经消失。
pub(crate) fn run_process_supervisor(
    path: &Path,
    allow_unmanaged_roles: bool,
) -> Result<(), String> {
    let config = read_runtime_config(path)?;
    let executable =
        std::env::current_exe().map_err(|error| format!("解析 qx-cli 可执行文件失败: {error}"))?;
    let work_dir = std::env::current_dir().map_err(|error| format!("读取工作目录失败: {error}"))?;
    supervise_workers(&config, path, &executable, &work_dir, allow_unmanaged_roles)
}

/// 供跨进程恢复验收器调用的极小子进程入口。
///
/// 子进程只操作持久化控制命令队列，不持有父进程内存状态；因此可以真实验证
/// 进程退出后租约过期接管、fencing token 递增、旧 worker 确认被拒绝以及新
/// worker 确认成功，而不是把这些场景缩小成同进程函数调用。
pub(crate) fn run_recovery_child(args: &[String]) -> Result<(), String> {
    let root = args
        .get(2)
        .ok_or_else(|| "recovery-child 缺少 queue-root".to_string())?;
    let action = args
        .get(3)
        .ok_or_else(|| "recovery-child 缺少 action".to_string())?;
    let owner = args
        .get(4)
        .ok_or_else(|| "recovery-child 缺少 owner".to_string())?;
    let now = args
        .get(5)
        .ok_or_else(|| "recovery-child 缺少 now".to_string())?
        .parse::<u64>()
        .map_err(|error| format!("recovery-child now 非法: {error}"))?;
    let queue = ControlCommandQueue::new(root);
    match action.as_str() {
        "claim" | "takeover" => {
            let lease_seconds = args
                .get(6)
                .ok_or_else(|| "recovery-child claim 缺少 lease_seconds".to_string())?
                .parse::<u64>()
                .map_err(|error| format!("recovery-child lease_seconds 非法: {error}"))?;
            let command_id = args
                .get(7)
                .ok_or_else(|| "recovery-child claim 缺少 command_id".to_string())?
                .parse::<u64>()
                .map_err(|error| format!("recovery-child command_id 非法: {error}"))?;
            let lease = queue
                .claim(command_id, owner, now, lease_seconds)
                .map_err(|error| format!("recovery-child {action} 失败: {error:?}"))?;
            println!("{}", lease.fencing_token);
        }
        "stale-ack" | "ack" => {
            let token = args
                .get(6)
                .ok_or_else(|| "recovery-child ack 缺少 fencing_token".to_string())?
                .parse::<u64>()
                .map_err(|error| format!("recovery-child fencing_token 非法: {error}"))?;
            let command_id = args
                .get(7)
                .ok_or_else(|| "recovery-child ack 缺少 command_id".to_string())?
                .parse::<u64>()
                .map_err(|error| format!("recovery-child command_id 非法: {error}"))?;
            let result = queue.ack_at(command_id, owner, token, now);
            if action == "stale-ack" {
                if result.is_ok() {
                    return Err("旧 worker fencing token 意外确认成功".into());
                }
                println!("rejected");
            } else {
                result.map_err(|error| format!("recovery-child ack 失败: {error:?}"))?;
                println!("acked");
            }
        }
        other => return Err(format!("recovery-child action 不支持: {other}")),
    }
    Ok(())
}

pub(crate) fn recovery_child_command(argv: &[String]) {
    let args = argv.to_vec();
    if let Err(error) = run_recovery_child(&args) {
        eprintln!("跨进程恢复子进程失败: {error}");
        std::process::exit(2);
    }
}

pub(crate) fn supervise_command(argv: &[String]) {
    let path = argv
        .get(2)
        .cloned()
        .unwrap_or_else(|| "deploy/qianxing.runtime.example.json".into());
    let allow_unmanaged_roles = argv
        .iter()
        .any(|argument| argument == "--allow-unmanaged-roles");
    if let Err(error) = run_process_supervisor(Path::new(&path), allow_unmanaged_roles) {
        eprintln!("进程监督器停止: {error}");
        std::process::exit(2);
    }
}
