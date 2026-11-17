//! CLI adapters for the runtime API, console, supervisor, and recovery child process.

use super::*;

pub(super) fn dispatch_serve(path: PathBuf) {
    if let Err(error) = run_runtime_api(&path) {
        eprintln!("[qx-cli · CLI] 运行时 API 启动失败: {error}");
        std::process::exit(2);
    }
}

pub(super) fn dispatch_console(args: crate::console_args::ConsoleArgs) {
    usage_errors::exit_on_failure(serve_console(&args), "控制台启动失败");
}

pub(super) fn dispatch_supervise(path: PathBuf, allow_unmanaged_roles: bool) {
    if let Err(failure) = run_process_supervisor(&path, allow_unmanaged_roles) {
        eprintln!("[qx-cli · CLI] 进程监督器停止: {failure}");
        match failure {
            qx_orchestrator::SuperviseFailure::Usage(_) => std::process::exit(2),
            qx_orchestrator::SuperviseFailure::WorkerExit { raw_code, .. } => {
                std::process::exit(match raw_code {
                    Some(code) if code != 0 => code,
                    _ => 128,
                })
            }
        }
    }
}

pub(super) fn dispatch_recovery_child(arguments: Vec<String>) {
    let mut argv = vec![String::new(), "recovery-child".to_string()];
    argv.extend(arguments);
    if let Err(error) = run_recovery_child(&argv) {
        eprintln!("[qx-cli · CLI] 跨进程恢复子进程失败: {error}");
        std::process::exit(2);
    }
}
