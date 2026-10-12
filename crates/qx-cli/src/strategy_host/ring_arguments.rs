use super::*;

/// 共享内存传输交给 worker 的启动实参：协议名、两条 ring 的路径与容量，以及父进程身份。
///
/// 拆成纯函数是为了让"worker 是否拿得到父进程 pid"有一条能咬的用例。这条传输没有 stdin
/// 可关，父进程被强杀时 Rust 侧的 `Drop` 不会跑，`--parent-pid` 一断，子进程的空闲循环
/// 就只剩 1 kHz 空转这一种结局（V13 R2 第十四遍 #215）。
pub(crate) fn shared_ring_arguments(
    transport: StrategyTransport,
    input_path: &Path,
    output_path: &Path,
    ring_config: SharedRingConfig,
    parent_pid: u32,
) -> Vec<String> {
    let protocol = if transport == StrategyTransport::SharedMemoryColumnar {
        "shared_memory_columnar"
    } else {
        "shared_memory_json"
    };
    vec![
        "--protocol".into(),
        protocol.into(),
        "--input-ring".into(),
        input_path.to_string_lossy().into_owned(),
        "--output-ring".into(),
        output_path.to_string_lossy().into_owned(),
        "--ring-capacity".into(),
        ring_config.capacity.to_string(),
        "--ring-slot-bytes".into(),
        ring_config.slot_bytes.to_string(),
        "--parent-pid".into(),
        parent_pid.to_string(),
    ]
}
