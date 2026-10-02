//! 回测产物必须落在**启动目录**的 `storage.data_dir` 树下，而不是 runtime.json 同级目录。
//!
//! 立案的是 V13 R2 第二十七遍 #255：可写运行态（账本、队列、outbox）、`report`/`status` 读侧
//! 与事件回测证据闸门全按进程当前目录取数，而产物曾按「相对配置文件目录」另立一条口径。
//! 同一份相对配置因此指两棵树——实盘闸门读不到自家回测写出的 `runs/*.run.json`，而文档里那条
//! 示例命令会把产物写进被 git 跟踪的 `deploy/data/**`。用例刻意不改写示例的 `data_dir`：
//! 改成绝对路径的示例两条口径重合，正好证不了这件事。

use std::path::{Path, PathBuf};
use std::process::Command;

const MANIFEST: &str = "qianxing.fast-backtest.ashare.example.json";
const RUNTIME: &str = "qianxing.runtime.ashare.example.json";
const DATA_DIR: &str = "data/qianxing-ashare";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("仓库根目录")
        .to_path_buf()
}

/// 把 `deploy/` 顶层的示例整体搬进 `<临时>/configs/`（**不**带上 `deploy/data/`），
/// 另建一个 `<临时>/launch/` 当启动目录。整层复制是因为示例之间还有兄弟引用
/// （runtime → DatasetBundle → 组件），漏一份就把用例变成"找不到输入"。
fn staged_examples(label: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!(
        "qianxing-backtest-artifact-root-{label}-{}",
        std::process::id()
    ));
    let launch = base.join("launch");
    let configs = base.join("configs");
    let _ = std::fs::remove_dir_all(&base);
    for dir in [&launch, &configs] {
        std::fs::create_dir_all(dir).expect("创建临时运行目录失败");
    }
    let deploy = repository_root().join("deploy");
    for entry in std::fs::read_dir(&deploy)
        .unwrap_or_else(|error| panic!("读取 deploy 目录失败: {error}"))
        .flatten()
    {
        let path = entry.path();
        if path.is_file() {
            std::fs::copy(
                &path,
                configs.join(path.file_name().expect("顶层条目有文件名")),
            )
            .unwrap_or_else(|error| panic!("复制示例失败 {}: {error}", path.display()));
        }
    }
    // 示例的相对落点是这条判据的前提，写法一改就要在这里同步。
    let runtime = std::fs::read_to_string(configs.join(RUNTIME)).expect("读取示例运行时配置失败");
    assert!(
        runtime.contains(&format!("\"data_dir\": \"{DATA_DIR}\"")),
        "示例的 data_dir 写法已变，用例需同步"
    );
    (launch, configs)
}

fn run(cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("启动 qx-cli 失败");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn summaries(runs_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(runs_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".summary.json"))
        })
        .collect()
}

#[test]
fn artifacts_land_beside_the_launch_directory_not_beside_the_config() {
    let (launch, configs) = staged_examples("landing");
    let manifest = configs.join(MANIFEST);
    let (code, stdout, stderr) = run(&launch, &["fast-backtest", &manifest.to_string_lossy()]);
    assert_eq!(code, 0, "A 股快速回测失败: {stderr}");
    let runs = launch.join(DATA_DIR).join("runs");
    assert!(
        !summaries(&runs).is_empty(),
        "产物没落在启动目录口径的 data_dir 下: {}（stdout={stdout} stderr={stderr}）",
        runs.display()
    );
    assert!(
        !configs.join("data").exists(),
        "产物又落在 runtime.json 同级目录了，文档那条示例命令会写脏配置目录: {}",
        configs.join("data").display()
    );
    let _ = std::fs::remove_dir_all(launch.parent().expect("临时目录有父目录"));
}

#[test]
fn report_and_the_launch_directory_agree_on_one_artifact_root() {
    let (launch, configs) = staged_examples("report");
    let manifest = configs.join(MANIFEST);
    let runtime = configs.join(RUNTIME);
    let (code, _, stderr) = run(&launch, &["fast-backtest", &manifest.to_string_lossy()]);
    assert_eq!(code, 0, "A 股快速回测失败: {stderr}");
    let (code, stdout, stderr) = run(&launch, &["report", &runtime.to_string_lossy()]);
    assert_eq!(
        code, 0,
        "写侧与读侧不同源，report 读不回刚跑出的摘要: {stderr}"
    );
    assert!(
        stdout.contains("000001.SZSE"),
        "报告没念出这一轮跑的标的: {stdout}"
    );
    // 换启动目录就是换一棵账本：这里必须如实报"这一棵树里没有摘要"，而不是替用户到
    // 配置目录那边挑一份——那正是 #255 混用过两棵树的读法。
    let (code, _, stderr) = run(&configs, &["report", &runtime.to_string_lossy()]);
    assert_ne!(code, 0, "换启动目录却读到了另一棵树的摘要");
    assert!(
        stderr.contains("未找到回测摘要"),
        "失败信息要点名找不到的那棵产物树，实际: {stderr}"
    );
    let _ = std::fs::remove_dir_all(launch.parent().expect("临时目录有父目录"));
}

/// `init` 生成的项目必须把落点钉在自己目录里，而不是继承模板那份仓库内的相对写法。
///
/// #255 的另一半：`storage.data_dir` 统一成「相对进程当前目录」之后，模板里的
/// `data/qianxing-*` 照抄进用户项目，等于把「先 cd 到项目目录」这条从未印在屏幕上的前提，
/// 变成首屏命令把产物写到使用者当时的目录里。init 因此把它改写成项目目录下的绝对落点。
///
/// 反向验证：去掉 `anchor_init_data_dir` 调用，本用例先红在「生成的 data_dir 仍是相对路径」，
/// 集成侧的四条 profile 也随之改判（见 `src/tests/init_onboarding.rs`）。
#[test]
fn init_projects_bake_their_own_artifact_root() {
    let base = std::env::temp_dir().join(format!(
        "qianxing-init-artifact-root-{}",
        std::process::id()
    ));
    let project = base.join("project");
    let elsewhere = base.join("elsewhere");
    let _ = std::fs::remove_dir_all(&base);
    for dir in [&project, &elsewhere] {
        std::fs::create_dir_all(dir).expect("创建临时项目目录失败");
    }
    let runtime = project.join("qianxing.runtime.json");
    let (code, stdout, stderr) = run(
        &elsewhere,
        &[
            "init",
            &runtime.to_string_lossy(),
            "--profile",
            "builtin",
            "--strategy",
            "macd",
        ],
    );
    assert_eq!(code, 0, "init 失败: {stdout} {stderr}");
    let payload = std::fs::read_to_string(&runtime).expect("读取生成的运行时配置失败");
    let document: serde_json::Value =
        serde_json::from_str(&payload).expect("生成的运行时配置不是合法 JSON");
    let configured = document["storage"]["data_dir"]
        .as_str()
        .expect("生成的运行时配置缺少 storage.data_dir");
    let data_dir = PathBuf::from(configured);
    assert!(
        data_dir.is_absolute(),
        "生成的 data_dir 仍是相对路径，产物落点取决于使用者当时在哪个目录: {configured}"
    );
    assert!(
        data_dir.starts_with(&project),
        "生成的 data_dir 不在项目目录里: {configured}"
    );
    // 首屏那条命令从别处启动也要把产物写进项目：这才是"任意 cwd 可用"的完整含义。
    let printed = stdout
        .lines()
        .find_map(|line| line.split('；').nth(1))
        .unwrap_or_else(|| panic!("init 没有印出首屏回测命令:\n{stdout}"));
    let args = printed
        .trim_start_matches("qx-cli ")
        .split_whitespace()
        .collect::<Vec<_>>();
    assert!(!args.is_empty(), "首屏回测命令为空: {printed}");
    let (code, _, stderr) = run(&elsewhere, &args);
    assert_eq!(code, 0, "从别的目录照抄首屏命令失败: {stderr}");
    assert!(
        !summaries(&data_dir.join("runs")).is_empty(),
        "产物没写进项目自己的 data_dir: {}",
        data_dir.join("runs").display()
    );
    assert!(
        !elsewhere.join("data").exists(),
        "产物又落到启动目录了: {}",
        elsewhere.join("data").display()
    );
    let _ = std::fs::remove_dir_all(&base);
}
