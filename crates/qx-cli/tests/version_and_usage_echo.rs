//! 版本出口与用法错误回显的契约（易用性 U1/U2/U4）。
//!
//! 三条 `version` 写法必须给同一行身份，用法错误必须只给分级回显而不是整篇入口摘要：
//! 改前实测两者都是 162 行 / 12 KB 的墙，且 `--version` 退 2。这里的用例逐条钉住改后的形状，
//! 任一退化（把摘要加回来、把某个别名摘掉、doctor 不再报身份）都会让本文件的某条用例变红。

use std::path::PathBuf;
use std::process::Command;

fn run(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(args)
        .output()
        .expect("启动 qx-cli 失败");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// 仓库根的运行时配置样例：集成用例的工作目录是 crate 根，必须给绝对路径。
fn example_runtime() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../deploy/qianxing.runtime.example.json")
        .canonicalize()
        .expect("deploy/qianxing.runtime.example.json 必须存在")
}

/// `version`、`--version`、`-V` 是同一条入口的三种写法：逐字相同的一行，退 0。
#[test]
fn three_version_spellings_print_one_identical_line() {
    let mut lines: Vec<String> = Vec::new();
    for spelling in ["version", "--version", "-V"] {
        let (code, stdout, stderr) = run(&[spelling]);
        assert_eq!(code, 0, "`qx-cli {spelling}` 必须退 0，stderr: {stderr}");
        assert!(stderr.is_empty(), "版本出口不该写 stderr: {stderr}");
        let printed: Vec<&str> = stdout.lines().collect();
        assert_eq!(
            printed.len(),
            1,
            "`qx-cli {spelling}` 必须只有一行（不带横幅），实际 {} 行",
            printed.len()
        );
        lines.push(printed[0].to_string());
    }
    assert_eq!(lines[0], lines[1], "--version 与 version 的身份行不同值");
    assert_eq!(lines[1], lines[2], "-V 与 --version 的身份行不同值");
}

/// 身份行的四格都必须来自构建期注入，硬编码版本或漏一格都在这里判红。
#[test]
fn identity_line_carries_version_commit_target_and_profile() {
    let (_, stdout, _) = run(&["version"]);
    let line = stdout.trim_end();
    let version = std::env!("CARGO_PKG_VERSION");
    assert!(
        line.starts_with(&format!("qianxing {version} (build ")),
        "身份行前缀不符: {line}"
    );
    for field in [", target ", ", profile "].iter() {
        assert!(line.contains(*field), "身份行缺格 {field}: {line}");
    }
    assert!(line.ends_with(')'), "身份行未闭合: {line}");
    let build = line
        .split("build ")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .expect("身份行没有 build 段");
    let revision = build.trim_end_matches("-dirty");
    assert_eq!(
        revision.len(),
        7,
        "提交号应截到 7 位（无 git 时为 unknown）: {build}"
    );
    assert!(build.len() <= 13, "build 段除提交号只允许 -dirty: {build}");
}

/// `doctor` 的第一格回答"我是哪个构建在说这句话"，且与 `version` 逐字同一份。
#[test]
fn doctor_reports_the_same_identity_line_as_version() {
    let identity = run(&["version"]);
    let runtime = example_runtime();
    let (code, stdout, stderr) = run(&["doctor", &runtime.display().to_string()]);
    assert_eq!(code, 0, "doctor 失败: {stderr}");
    assert!(
        stdout.contains(&format!("[PASS] build_identity: {}", identity.1.trim_end())),
        "doctor 未报出与 version 同一行的身份:\n{stdout}"
    );
    assert!(
        stdout.contains("[PASS] config:"),
        "doctor 原有的配置格不得消失:\n{stdout}"
    );
}

/// 用法错误是一页纸，不是一面墙：退出码仍是 2，stdout 不再有入口摘要。
#[test]
fn usage_error_echoes_graded_lines_instead_of_the_entry_summary() {
    let (code, stdout, stderr) = run(&["nope"]);
    assert_eq!(code, 2, "未知命令必须退出 2");
    assert!(
        stdout.is_empty(),
        "用法错误不得再把入口摘要打到 stdout（实测 {} 行）",
        stdout.lines().count()
    );
    let echoed = stderr.lines().count();
    assert!(
        echoed <= 10,
        "回显必须压在 10 行内，实际 {echoed} 行:\n{stderr}"
    );
    assert!(stderr.contains("未知命令或未知参数"), "{stderr}");
    assert!(stderr.contains("下一步"), "必须给出下一步指路: {stderr}");
    assert!(stderr.contains("自证构建"), "必须指到版本出口: {stderr}");
    assert!(
        !stderr.contains("配置与运维入口"),
        "不得再打印整篇摘要: {stderr}"
    );
}

/// 拼错的入口名由 clap 点名近似项；形状错的入口给出该入口自己的用法。
#[test]
fn misspelled_and_misshaped_entries_get_their_own_hint() {
    let (code, _, stderr) = run(&["versoin"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("'version'"),
        "近似入口名必须点名 version: {stderr}"
    );

    let (code, _, stderr) = run(&["doctor", "--config", "x.json"]);
    assert_eq!(code, 2);
    assert!(
        stderr.contains("doctor [OPTIONS] [PATH]"),
        "doctor 不收 --config 时必须印出它自己的形状: {stderr}"
    );
}

/// 机器读面与人工读面同口径：`status --json` 与 `report --json` 都带 runtime_version。
#[test]
fn json_surfaces_carry_the_same_runtime_version_as_version_entry() {
    let version = run(&["version"]).1;
    let declared = version
        .trim_start_matches("qianxing ")
        .split_whitespace()
        .next()
        .expect("身份行首格是版本号")
        .to_string();
    let needle = format!("\"runtime_version\": \"{declared}\"");

    let runtime = example_runtime();
    let (code, stdout, stderr) = run(&["status", "--json", &runtime.display().to_string()]);
    assert_eq!(code, 0, "status --json 失败: {stderr}");
    assert!(
        stdout.contains(&needle),
        "status JSON 未带 {needle}:\n{stdout}"
    );

    // 报告侧只需一份最小摘要：没有 input 块时按"未作声明"如实回显，不影响身份格。
    let summary = std::env::temp_dir().join("qx-pass28-identity.summary.json");
    std::fs::write(&summary, "{}\n").expect("写入临时回测摘要失败");
    let (code, stdout, stderr) = run(&["report", "--json", &summary.display().to_string()]);
    let _ = std::fs::remove_file(&summary);
    assert_eq!(code, 0, "report --json 失败: {stderr}");
    assert!(
        stdout.contains(&needle),
        "report JSON 未带 {needle}:\n{stdout}"
    );
    assert!(
        stdout.contains("\"not_declared\""),
        "无 input 块的摘要必须回显未声明:\n{stdout}"
    );
}
