//! `qx-cli quickstart` 的首跑契约（易用性 P2 第一格 / #260 #261）。
//!
//! 三条判据都来自本轮实测，不是设计愿望：
//! - 一条命令等于 README 那五条入口逐条敲：退出码 0、五句 `[完成]` 按顺序出现、项目里 14 份
//!   文件，且 `result_hash` 与同一输入的直跑链**逐字相等**。quickstart 直调同一批函数，所以这条
//!   相等性是结构性的——一旦有人把它改成"另起一套实现"，先在这里判红。
//! - #260 屏幕上印出的程序名必须就是这台机器上敲得动的那个：判据取测试二进制自身的文件名，
//!   不在这里再抄一份字面量。`qx-cli version` 那行的 `qianxing …` 是产品身份词，由
//!   `version_and_usage_echo.rs` 钉住，与这里的命令行前缀不是一回事。
//! - #261 指路的三条命令照抄都要退 0：这一格的收尾指路曾是 `paper-check <macd 项目>`，
//!   实测以 2 退出（Paper 主链路缺少启用的 Scheduler worker）。
//!
//! 失败面只钉真实可复现的那一步：不带 `--force` 重跑必然撞在第 1 步，此时回显必须是那一步的
//! 原文命令加重跑整条的写法，并以 2 退出，且一个已有文件都不许动。

use std::path::{Path, PathBuf};
use std::process::Command;

/// `init` 落 9 份 + 首轮回测落 5 份（数据集清单与 runs 四份产物）；这份分解由
/// `src/tests/init_onboarding.rs` 的 `init_lands_nine_files_and_the_advertised_backtest_adds_five`
/// 逐文件钉住，两处口径必须一起改。
const EXPECTED_PROJECT_FILES: usize = 14;

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("仓库根目录")
        .to_path_buf()
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_qx-cli"))
}

/// #260 的判据口径：这台机器上真正敲得动的那个名字。
fn program() -> String {
    binary()
        .file_stem()
        .expect("测试二进制有文件名")
        .to_string_lossy()
        .into_owned()
}

/// 临时目录按用例名与进程号取：集成用例并行跑，共用一个目录会互相踩产物。
fn temp_base(label: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "qianxing-quickstart-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("创建临时运行目录失败");
    base
}

/// 一律从 `cwd` 启动真 binary：首跑链条的落点判据就是"启动目录不该被写脏"。
fn run(cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(binary())
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

fn count_files(dir: &Path) -> usize {
    let mut stack = vec![dir.to_path_buf()];
    let mut files = 0_usize;
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files += 1;
            }
        }
    }
    files
}

/// 屏幕上出现过的结果指纹集合：一轮链条只该有一个 `result_hash`。
fn result_hashes(text: &str) -> Vec<String> {
    let mut hashes = text
        .split_whitespace()
        .filter(|token| token.starts_with("result_hash="))
        .map(|token| token.trim_start_matches("result_hash=").to_string())
        .collect::<Vec<_>>();
    hashes.sort();
    hashes.dedup();
    hashes
}

/// 取「下一步三条命令」那一栏：只有缩进两格、且以这台机器上真敲得动的程序名开头的行才算指路。
fn advertised_next_steps(text: &str) -> Vec<String> {
    let position = text
        .find("下一步三条命令：")
        .unwrap_or_else(|| panic!("quickstart 没有印出下一步指路:\n{text}"));
    let indent = format!("  {} ", program());
    text[position..]
        .lines()
        .skip(1)
        .take_while(|line| line.starts_with(indent.as_str()))
        .map(|line| line.trim_start().to_string())
        .collect()
}

/// 一条命令 == 逐条敲：五步、份数、落点与结果指纹逐项对照第二条链。
#[test]
fn one_command_lands_the_same_chain_as_the_command_by_command_chain() {
    let base = temp_base("chain");
    let project = base.join("project");
    let project_text = project.to_string_lossy().into_owned().replace('\\', "/");
    let deploy_data = repository_root().join("deploy").join("data");
    let deploy_before = count_files(&deploy_data);
    let (code, stdout, stderr) = run(&base, &["quickstart", &project.to_string_lossy()]);
    assert_eq!(code, 0, "quickstart 首跑失败: {stderr}");
    let names = program();
    let done = stdout
        .lines()
        .filter(|line| line.starts_with("[完成] "))
        .collect::<Vec<_>>();
    let expected = [
        ("建项目", "init"),
        ("静态检查", "doctor"),
        ("跑一轮回测", "backtest"),
        ("读回摘要", "report"),
        ("看安全状态", "status"),
    ];
    assert_eq!(
        done.len(),
        expected.len(),
        "首跑必须正好印五句 [完成]，实际 {} 句:\n{stdout}",
        done.len()
    );
    for (index, (label, entry)) in expected.iter().enumerate() {
        assert!(
            done[index].starts_with(&format!("[完成] {label}：{names} {entry} ")),
            "第 {} 步的落点不对: {}",
            index + 1,
            done[index]
        );
    }
    assert_eq!(
        count_files(&project),
        EXPECTED_PROJECT_FILES,
        "首跑落盘份数与文档口径不符：{} 里 {} 份",
        project.display(),
        count_files(&project)
    );
    // 屏幕上声明的每一份产物落点都必须在项目里：裸相对路径或写到启动目录都算跑偏。
    let mut declared = 0_usize;
    for token in stdout.split_whitespace() {
        let Some(key) = ["summary=", "equity=", "fills=", "path=", "data_dir="]
            .iter()
            .find(|key| token.starts_with(**key))
        else {
            continue;
        };
        let value = &token[key.len()..];
        // 同名键也挂在计数字段上（`[Strategy · Backtest] … fills=1`），只把带目录分隔符的
        // 值当落点判：相对路径同样含分隔符，所以"产物写到别处"仍然咬得住。
        if !value.contains('/') && !value.contains('\\') {
            continue;
        }
        assert!(
            value.replace('\\', "/").starts_with(&project_text),
            "产物写到了使用者指定的目录之外: {token}"
        );
        declared += 1;
    }
    assert!(
        declared >= 4,
        "首跑至少要声明 summary/equity/fills/run.json 四份落点，实际 {declared} 份:\n{stdout}"
    );
    assert!(
        !base.join("data").exists(),
        "启动目录被写了产物: {}",
        base.join("data").display()
    );
    assert_eq!(
        count_files(&deploy_data),
        deploy_before,
        "quickstart 弄脏了仓库的 deploy/data（发布口径要求工作树只读）"
    );
    let hashes = result_hashes(&stdout);
    assert_eq!(
        hashes.len(),
        1,
        "一条链条只该有一个 result_hash，实际 {hashes:?}"
    );
    // 对照链在项目目录之外另起一棵：`temp_base` 只建了 base，这一层得自己建。
    let direct = base.join("direct");
    std::fs::create_dir_all(&direct).expect("创建对照链目录失败");
    let direct_runtime = direct.join("qianxing.runtime.json");
    let (code, _, stderr) = run(
        &direct,
        &[
            "init",
            &direct_runtime.to_string_lossy(),
            "--strategy",
            "macd",
        ],
    );
    assert_eq!(code, 0, "逐条敲的对照链 init 失败: {stderr}");
    let (code, direct_stdout, stderr) =
        run(&direct, &["backtest", &direct_runtime.to_string_lossy()]);
    assert_eq!(code, 0, "逐条敲的对照链 backtest 失败: {stderr}");
    assert_eq!(
        result_hashes(&direct_stdout),
        hashes,
        "quickstart 的结果指纹与同一输入的逐条敲不等"
    );
    let _ = std::fs::remove_dir_all(base);
}

/// 指路的三条命令逐条照抄必须退 0（#261）。
#[test]
fn advertised_next_steps_run_as_printed() {
    let base = temp_base("hints");
    let project = base.join("project");
    let (code, stdout, stderr) = run(&base, &["quickstart", &project.to_string_lossy()]);
    assert_eq!(code, 0, "quickstart 首跑失败: {stderr}");
    let steps = advertised_next_steps(&stdout);
    assert_eq!(steps.len(), 3, "下一步指路必须是三条:\n{stdout}");
    for step in &steps {
        let args = step.split_whitespace().skip(1).collect::<Vec<_>>();
        for argument in &args {
            assert!(
                !argument.contains(' '),
                "参数含空白，切分会失真: {argument}"
            );
        }
        let (code, _, stderr) = run(&base, &args);
        assert_eq!(code, 0, "指路命令照抄跑不通: {step}\n{stderr}");
    }
    assert!(
        !steps.iter().any(|step| step.contains("paper-check")),
        "paper-check 不属于这条链的指路命令: {steps:?}"
    );
    assert!(
        stdout.contains("paper-check") && stdout.contains("paper profile"),
        "收尾句要如实交代 paper-check 的前置条件，而不是把它当成一条能敲的命令:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(base);
}

/// 止步的那一步：回显原文命令、以 2 退出、不动已有文件；`--force` 是唯一的自愈出口。
#[test]
fn second_run_without_force_stops_at_the_first_step() {
    let base = temp_base("stop");
    let project = base.join("project");
    let (code, _, stderr) = run(&base, &["quickstart", &project.to_string_lossy()]);
    assert_eq!(code, 0, "quickstart 首跑失败: {stderr}");
    let files_before = count_files(&project);
    let names = program();
    let runtime = project.join("qianxing.runtime.json");
    let (code, stdout, stderr) = run(&base, &["quickstart", &project.to_string_lossy()]);
    assert_eq!(
        code, 2,
        "已存在的项目不带 --force 重跑必须止步并以 2 退出:\n{stdout}"
    );
    assert!(
        stderr.contains("[止步] 建项目 失败"),
        "止步文案要点名失败的那一步: {stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "这一步的原文命令：{names} init {}",
            runtime.display()
        )),
        "必须回显那一步的原文命令: {stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "重跑整条链条：{names} quickstart {}",
            project.display()
        )),
        "必须给出重跑整条链条的写法: {stderr}"
    );
    assert!(
        !stdout.contains("[完成] "),
        "止步之后不许再印完成行: {stdout}"
    );
    assert_eq!(
        count_files(&project),
        files_before,
        "止步的那一步动了已有文件"
    );
    let (code, stdout, stderr) = run(
        &base,
        &["quickstart", &project.to_string_lossy(), "--force"],
    );
    assert_eq!(code, 0, "--force 必须把整条链条重走完: {stderr}");
    assert_eq!(
        stdout.matches("[完成] ").count(),
        5,
        "--force 重跑的五步不齐:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(base);
}
