//! 运行编排边界。
//!
//! 该 crate 只负责把已验证的 RuntimeConfig 转换为 worker 启动计划，以及管理
//! worker 子进程的日志、退出传播和停止顺序；不执行策略、下单、对账或账簿副作用。

mod supervisor_stop;

use qx_core::VenueFamily;
use qx_runtime::{RuntimeConfig, WorkerRole};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use supervisor_stop::{wait_for_children, ManagedProcess, SupervisorStop};

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorkerLaunch {
    pub worker_id: String,
    pub args: Vec<String>,
}

fn resolve_ccxt_config_path(runtime_path: &Path, configured: &str) -> String {
    let path = Path::new(configured);
    if path.is_absolute() {
        return path.to_string_lossy().into_owned();
    }
    runtime_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(path)
        .to_string_lossy()
        .into_owned()
}

/// 根据 RuntimeConfig 生成确定性的 worker 启动计划。
pub fn plan_workers(
    config: &RuntimeConfig,
    config_path: &Path,
    allow_unmanaged_roles: bool,
) -> Result<Vec<WorkerLaunch>, String> {
    let mut launches = Vec::new();
    let mut unmanaged = Vec::new();
    let config_arg = config_path.to_string_lossy().into_owned();
    for worker in config.workers.iter().filter(|worker| worker.enabled) {
        let args = match worker.role {
            WorkerRole::Api => vec!["serve".into(), config_arg.clone()],
            WorkerRole::MarketData => {
                if let Some(ccxt_config) = worker.endpoint.as_deref() {
                    vec![
                        "ccxt-worker".into(),
                        config_arg.clone(),
                        worker.id.clone(),
                        resolve_ccxt_config_path(config_path, ccxt_config),
                    ]
                } else if VenueFamily::parse_option(worker.venue_id.as_deref())
                    == Some(VenueFamily::Binance)
                {
                    vec![
                        "binance-worker".into(),
                        config_arg.clone(),
                        worker.id.clone(),
                    ]
                } else {
                    unmanaged.push(worker.id.clone());
                    continue;
                }
            }
            WorkerRole::UserStream => {
                if let Some(ccxt_config) = worker.endpoint.as_deref() {
                    vec![
                        "ccxt-worker".into(),
                        config_arg.clone(),
                        worker.id.clone(),
                        resolve_ccxt_config_path(config_path, ccxt_config),
                    ]
                } else if VenueFamily::parse_option(worker.venue_id.as_deref())
                    == Some(VenueFamily::Binance)
                {
                    vec![
                        "binance-worker".into(),
                        config_arg.clone(),
                        worker.id.clone(),
                    ]
                } else {
                    unmanaged.push(worker.id.clone());
                    continue;
                }
            }
            WorkerRole::Execution => {
                if VenueFamily::parse_option(worker.venue_id.as_deref()) == Some(VenueFamily::Paper)
                {
                    vec!["paper-worker".into(), config_arg.clone(), worker.id.clone()]
                } else if let Some(ccxt_config) = worker.endpoint.as_deref() {
                    vec![
                        "ccxt-worker".into(),
                        config_arg.clone(),
                        worker.id.clone(),
                        resolve_ccxt_config_path(config_path, ccxt_config),
                    ]
                } else if VenueFamily::parse_option(worker.venue_id.as_deref())
                    == Some(VenueFamily::Binance)
                {
                    vec![
                        "binance-worker".into(),
                        config_arg.clone(),
                        worker.id.clone(),
                    ]
                } else {
                    unmanaged.push(worker.id.clone());
                    continue;
                }
            }
            WorkerRole::SpreadRecovery => {
                if VenueFamily::parse_option(worker.venue_id.as_deref()) == Some(VenueFamily::Paper)
                {
                    vec!["paper-worker".into(), config_arg.clone(), worker.id.clone()]
                } else if let Some(ccxt_config) = worker.endpoint.as_deref() {
                    vec![
                        "ccxt-worker".into(),
                        config_arg.clone(),
                        worker.id.clone(),
                        resolve_ccxt_config_path(config_path, ccxt_config),
                    ]
                } else if VenueFamily::parse_option(worker.venue_id.as_deref())
                    == Some(VenueFamily::Binance)
                {
                    vec![
                        "binance-worker".into(),
                        config_arg.clone(),
                        worker.id.clone(),
                    ]
                } else {
                    unmanaged.push(worker.id.clone());
                    continue;
                }
            }
            WorkerRole::Reconciler => {
                if let Some(ccxt_config) = worker.endpoint.as_deref() {
                    vec![
                        "ccxt-worker".into(),
                        config_arg.clone(),
                        worker.id.clone(),
                        resolve_ccxt_config_path(config_path, ccxt_config),
                    ]
                } else if VenueFamily::parse_option(worker.venue_id.as_deref())
                    == Some(VenueFamily::Binance)
                {
                    vec![
                        "binance-worker".into(),
                        config_arg.clone(),
                        worker.id.clone(),
                    ]
                } else {
                    unmanaged.push(worker.id.clone());
                    continue;
                }
            }
            WorkerRole::Scheduler => vec![
                "scheduler-worker".into(),
                config_arg.clone(),
                worker.id.clone(),
            ],
            WorkerRole::Strategy => vec![
                "strategy-worker".into(),
                config_arg.clone(),
                worker.id.clone(),
            ],
            WorkerRole::OutboxRelay => vec![
                "outbox-relay-worker".into(),
                config_arg.clone(),
                worker.id.clone(),
            ],
            WorkerRole::EventConsumer => vec![
                "event-consumer-worker".into(),
                config_arg.clone(),
                worker.id.clone(),
            ],
        };
        launches.push(WorkerLaunch {
            worker_id: worker.id.clone(),
            args,
        });
    }
    if !unmanaged.is_empty() && !allow_unmanaged_roles {
        return Err(format!(
            "enabled roles have no built-in process entrypoint: {}; use --allow-unmanaged-roles only for externally managed adapters",
            unmanaged.join(", ")
        ));
    }
    if launches.is_empty() {
        return Err("运行时配置没有可托管的 enabled worker".into());
    }
    Ok(launches)
}

struct ManagedChild {
    id: String,
    child: Child,
}

impl ManagedProcess for ManagedChild {
    fn id(&self) -> &str {
        &self.id
    }

    fn poll_exit(&mut self) -> Result<Option<String>, String> {
        let status = self
            .child
            .try_wait()
            .map_err(|error| format!("检查 worker {} 状态失败: {error}", self.id))?;
        Ok(status.map(|status| {
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".into())
        }))
    }
}

fn stop_managed_children(children: &mut [ManagedChild]) {
    for managed in children.iter_mut() {
        let _ = managed.child.kill();
    }
    for managed in children.iter_mut() {
        let _ = managed.child.wait();
    }
}

/// 启动并监督一组 worker 子进程。任一 worker 异常退出时停止其余 worker。
pub fn supervise_workers(
    config: &RuntimeConfig,
    config_path: &Path,
    executable: &Path,
    work_dir: &Path,
    allow_unmanaged_roles: bool,
) -> Result<(), String> {
    let launches = plan_workers(config, config_path, allow_unmanaged_roles)?;
    // 在派生任何子进程之前就接管终止信号：否则规划/启动窗口内的 Ctrl+C 会直接打死
    // 监督器，留下无人回收的 worker 子进程。
    qx_runtime::install_shutdown_signals();
    let data_dir = Path::new(&config.storage.data_dir);
    let data_dir = if data_dir.is_absolute() {
        data_dir.to_path_buf()
    } else {
        work_dir.join(data_dir)
    };
    let log_dir = data_dir.join("process-logs");
    std::fs::create_dir_all(&log_dir)
        .map_err(|error| format!("创建进程日志目录失败 {}: {error}", log_dir.display()))?;

    let mut children = Vec::with_capacity(launches.len());
    let result = (|| -> Result<(), String> {
        for launch in launches {
            let stdout_path = log_dir.join(format!("{}.out.log", launch.worker_id));
            let stderr_path = log_dir.join(format!("{}.err.log", launch.worker_id));
            let stdout = std::fs::File::create(&stdout_path).map_err(|error| {
                format!("创建 worker {} stdout 日志失败: {error}", launch.worker_id)
            })?;
            let stderr = std::fs::File::create(&stderr_path).map_err(|error| {
                format!("创建 worker {} stderr 日志失败: {error}", launch.worker_id)
            })?;
            let child = Command::new(executable)
                .args(launch.args)
                .current_dir(work_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::from(stdout))
                .stderr(Stdio::from(stderr))
                .spawn()
                .map_err(|error| format!("启动 worker {} 失败: {error}", launch.worker_id))?;
            println!(
                "[监督器] started worker={} pid={}",
                launch.worker_id,
                child.id()
            );
            children.push(ManagedChild {
                id: launch.worker_id,
                child,
            });
        }
        let started = std::time::Instant::now();
        match wait_for_children(
            &mut children,
            qx_runtime::shutdown_signalled,
            || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            |millis| thread::sleep(Duration::from_millis(millis)),
            config.shutdown_timeout_ms,
        )? {
            SupervisorStop::WorkerExited { id, code } => {
                return Err(format!(
                    "managed worker {id} exited ({code}); stopping remaining workers"
                ))
            }
            SupervisorStop::StoppedWithinBudget { waited_ms } => {
                println!("[监督器 · Shutdown] 全部 worker 按停机请求退出 waited_ms={waited_ms}")
            }
            SupervisorStop::StopTimedOut {
                waited_ms,
                remaining,
            } => {
                return Err(format!(
                    "{remaining} 个 worker 收到停机请求后 {waited_ms}ms 仍未退出，超过 shutdown_timeout_ms"
                ))
            }
        }
        Ok(())
    })();
    stop_managed_children(&mut children);
    result
}

#[cfg(test)]
mod tests;
