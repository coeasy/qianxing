//! Runtime path arguments are part of the public CLI contract.

use std::process::Command;

/// Worker and submit-order commands must require their runtime config explicitly.
#[test]
fn runtime_path_positionals_fail_closed_when_omitted() {
    const ENTRIES: [(&str, &[&str]); 9] = [
        ("scheduler-worker", &["worker-1"]),
        ("strategy-worker", &["worker-1"]),
        ("paper-worker", &["worker-1"]),
        ("binance-worker", &["worker-1"]),
        ("ccxt-worker", &["worker-1", "ccxt.json"]),
        ("outbox-relay-worker", &["worker-1"]),
        ("event-consumer-worker", &["worker-1"]),
        ("binance-submit-order", &["worker-1", "submit-order.json"]),
        ("paper-submit-order", &["submit-order.json"]),
    ];

    for (entry, tail) in ENTRIES {
        let mut command = Command::new(env!("CARGO_BIN_EXE_qx-cli"));
        command.arg(entry).args(tail);
        let output = command.output().expect("启动 qx-cli 失败");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{entry} 缺少 runtime 路径必须是用法错误，而不能回落到示例配置:\n{stderr}"
        );
        assert!(
            stderr.contains("PATH"),
            "{entry} 的缺参提示要点名 runtime 路径:\n{stderr}"
        );

        let mut command = Command::new(env!("CARGO_BIN_EXE_qx-cli"));
        command.arg(entry).arg("deploy/runtime.json").args(tail);
        let output = command.output().expect("启动 qx-cli 失败");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("Usage:"),
            "{entry} 显式给出 runtime 路径后必须进入配置读取链，而不是仍报缺参:\n{stderr}"
        );
    }
}
