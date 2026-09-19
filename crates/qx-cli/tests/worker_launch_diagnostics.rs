//! 跨语言 worker 的启动失败必须能自证原因：是哪个程序、它从哪来、子进程状态与 stderr。
//!
//! 本机 `python` 可能只是 WindowsApps 的占位桩（实测 `python --version` 两个流都空、退出码 49），
//! 而策略代码报错也可能是"一个字都没输出就退出"。两种情况此前在协议层是同一句
//! "Strategy worker 已关闭输出"，所以这里钉住失败信息必须带上的字段。
//! 回测产物写进系统临时目录（改写 `storage.data_dir`），不落到仓库工作树。

use std::path::{Path, PathBuf};
use std::process::Command;

const RUNTIME_TEMPLATE: &str = "qianxing.runtime.strategy-backtest.example.json";
const BAR_TEMPLATE: &str = "qianxing.bar-frame.example.json";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("仓库根目录")
        .to_path_buf()
}

/// 把运行时模板复制到独立的临时运行目录，并把产物根改写到那里。
fn isolated_runtime(label: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "qianxing-worker-diagnostic-{label}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("创建临时运行目录失败");
    let template = repository_root().join("deploy").join(RUNTIME_TEMPLATE);
    let text = std::fs::read_to_string(template).expect("读取运行时模板失败");
    let redirected = text.replace(
        "\"data_dir\": \"data/qianxing-strategy-backtest\"",
        &format!(
            "\"data_dir\": \"{}\"",
            root.join("data").to_string_lossy().replace('\\', "/")
        ),
    );
    assert_ne!(
        redirected, text,
        "运行时模板的 data_dir 写法已变，用例需同步"
    );
    let runtime = root.join("runtime.json");
    std::fs::write(&runtime, redirected).expect("写入临时运行时配置失败");
    (runtime, root)
}

fn run_with_interpreter(runtime: &Path, interpreter: &str) -> (i32, String) {
    let bars = repository_root().join("deploy").join(BAR_TEMPLATE);
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(runtime.parent().expect("临时配置必须有父目录"))
        .env("QX_PYTHON", interpreter)
        .args([
            "strategy",
            "backtest",
            &runtime.to_string_lossy(),
            &bars.to_string_lossy(),
        ])
        .output()
        .expect("启动 qx-cli 失败");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// 解释器不存在时，失败信息必须点名是哪个程序无法执行，而不是只报"启动 worker 失败"。
#[test]
fn missing_interpreter_is_named_in_the_launch_failure() {
    let (runtime, _root) = isolated_runtime("missing");
    let (code, stderr) = run_with_interpreter(&runtime, "qx-4n-no-such-interpreter");
    assert_eq!(code, 2, "解释器不可用必须 fail-closed: {stderr}");
    assert!(
        stderr.contains("qx-4n-no-such-interpreter") && stderr.contains("无法执行"),
        "启动失败必须点名解释器:\n{stderr}"
    );
}

/// 解释器存在但不产出协议响应（这里用测试二进制自己当解释器，它只写 stderr）时，
/// 失败信息必须带上程序来源、子进程状态与 stderr 尾部。
#[test]
fn silent_worker_reports_program_origin_and_stderr() {
    let (runtime, _root) = isolated_runtime("silent");
    let interpreter = std::env::current_exe()
        .expect("测试二进制路径不可用")
        .to_string_lossy()
        .into_owned();
    let (code, stderr) = run_with_interpreter(&runtime, &interpreter);
    assert_eq!(code, 2, "worker 无响应必须 fail-closed: {stderr}");
    assert!(
        stderr.contains("程序=") && stderr.contains("来自 QX_PYTHON"),
        "失败信息必须说明是哪个解释器:\n{stderr}"
    );
    assert!(
        stderr.contains("退出码") || stderr.contains("进程未退出"),
        "失败信息必须说明子进程状态:\n{stderr}"
    );
    assert!(
        stderr.contains("stderr="),
        "失败信息必须带上 worker 的 stderr 尾部:\n{stderr}"
    );
}

/// 未设置 `QX_PYTHON` 时的回落本身也要出现在失败信息里——本机占位桩与 PATH 上真的
/// `python` 是两种环境，只有前者会失败，所以后者成功时不检查文本。
#[test]
fn fallback_interpreter_names_its_source_when_it_fails() {
    let (runtime, _root) = isolated_runtime("fallback");
    let bars = repository_root().join("deploy").join(BAR_TEMPLATE);
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(runtime.parent().expect("临时配置必须有父目录"))
        .env_remove("QX_PYTHON")
        .args([
            "strategy",
            "backtest",
            &runtime.to_string_lossy(),
            &bars.to_string_lossy(),
        ])
        .output()
        .expect("启动 qx-cli 失败");
    if output.status.code() == Some(0) {
        return;
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        stderr.contains("QX_PYTHON 未设置，回落 PATH python"),
        "回落必须被明确报告，不能让人以为配置里写了解释器:\n{stderr}"
    );
}
