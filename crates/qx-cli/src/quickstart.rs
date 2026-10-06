//! `quickstart` —— 把《工业化易用性收口指南》§1 的首跑链条收成一条命令（易用性 P2 第一格）。
//!
//! `init --strategy macd` → `doctor` → `backtest` → `report` → `status` 这五条此前只写在 README
//! 里靠人逐条敲：单条命令的报错一直有出口，但"整条链条走到第几步、失败那步的原文是什么、
//! 跑完接着敲什么"没有（U4 的成功侧）。本模块直调 crate 根上那五个入口的同一批函数，所以
//! 产物、退出码与 `result_hash` 都与手工逐条敲同源 —— 另起子进程只会多付五次启动，
//! 不会多证明任何东西。
//!
//! 落点口径：`init` 把 `storage.data_dir` 钉成项目目录内的绝对路径，因此整条链条只写使用者
//! 指定的目录，仓库的 `deploy/data/` 一份都不碰。

use super::*;
use std::path::{Path, PathBuf};

/// 不带参数时的项目目录名。默认值不取 `.`：那会把整份产物摊进使用者的当前目录，
/// 在仓库根执行就成了一次"首跑命令弄脏工作树"。
const DEFAULT_PROJECT_DIR: &str = "qianxing-quickstart";
/// 首跑绑定的内置策略与 README「安装 A」那六条里的 `--strategy macd` 一致。
const QUICKSTART_STRATEGY: &str = "macd";
/// 项目内运行时配置的文件名，同样取自 README 那六条的写法。
const RUNTIME_FILE_NAME: &str = "qianxing.runtime.json";
/// 收尾计数最多打开多少个目录。正常项目是一层 data 目录，个位数够用；这一格是符号链接环
/// 与误指大目录树的止损线，不是产品口径，所以取一个正常首跑永远碰不到的数。
pub(crate) const PROJECT_FILE_COUNT_BUDGET: usize = 2000;

/// 把一步的参数拼成可以原样粘贴回终端的一行；含空白的参数加引号。
/// 程序名不另抄一份：用的就是 `cli_args.rs` 里命令表声明的那一个常量。
fn command_line(arguments: &[&str]) -> String {
    let mut line = cli_args::PROGRAM_NAME.to_string();
    for argument in arguments {
        line.push(' ');
        if argument.contains(' ') {
            line.push('"');
            line.push_str(argument);
            line.push('"');
        } else {
            line.push_str(argument);
        }
    }
    line
}

/// 一步的落点：成功印一行「[完成]」；失败印「[止步]」+ 那一步的命令原文 + 重跑整条的写法，
/// 再以退出码 2 fail closed。止步的那一步必须能单独重跑，这是本入口唯一的错误面。
fn require(label: &str, command: &str, retry: &str, outcome: Result<(), String>) {
    if let Err(error) = outcome {
        eprintln!("[止步] {label} 失败：{error}");
        eprintln!("  这一步的原文命令：{command}");
        eprintln!("  修好这一条之后重跑整条链条：{retry}");
        std::process::exit(2);
    }
    println!("[完成] {label}：{command}");
}

/// 数项目目录里的文件份数，最多打开 `budget` 个目录：返回（份数，是否在预算处截断）。
///
/// 预算是为了让收尾陈述能在符号链接目录环上终止：`is_dir()` 跟随符号链接，一个指回祖先的
/// 链接就能让这里永不返回——五步产品全部跑完之后卡在「共 N 份文件」这一行上。同一份预算
/// 也给一次误指向大目录树的首跑兜底。
/// 文档口径是 9 份（`init`）+ 5 份（首跑回测）= 14 份。读不动的目录只少计一份，不让一次
/// 已经成功的首跑改口——这格计数是收尾陈述，不是判据，所以截断如实印出来而不是报错。
pub(crate) fn project_file_count(dir: &Path, budget: usize) -> (usize, bool) {
    let mut stack = vec![dir.to_path_buf()];
    let mut files = 0_usize;
    let mut opened = 0_usize;
    while let Some(current) = stack.pop() {
        opened += 1;
        if opened > budget {
            return (files, true);
        }
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
    (files, false)
}

/// `qx-cli quickstart [目录] [--force]` 的实现体：失败自行以 2 退出，成功印收尾指引后返回。
pub(crate) fn run(project: Option<PathBuf>, force: bool) {
    let root = project.unwrap_or_else(|| PathBuf::from(DEFAULT_PROJECT_DIR));
    let absolute = std::path::absolute(&root).unwrap_or_else(|_| root.clone());
    let runtime = absolute.join(RUNTIME_FILE_NAME);
    let runtime_text = runtime.to_string_lossy().into_owned();
    let root_text = absolute.to_string_lossy().into_owned();
    let retry = if force {
        command_line(&["quickstart", &root_text, "--force"])
    } else {
        command_line(&["quickstart", &root_text])
    };
    println!(
        "quickstart 依次执行 5 步（建项目 → 静态检查 → 回测 → 读回摘要 → 安全状态），只写 {}：",
        absolute.display()
    );
    let mut init_arguments = vec![
        "init",
        runtime_text.as_str(),
        "--strategy",
        QUICKSTART_STRATEGY,
    ];
    if force {
        init_arguments.push("--force");
    }
    require(
        "建项目",
        &command_line(&init_arguments),
        &retry,
        run_init_with_profile(&runtime, force, Some(QUICKSTART_STRATEGY), None),
    );
    require(
        "静态检查",
        &command_line(&["doctor", &runtime_text]),
        &retry,
        run_doctor(&runtime, false),
    );
    require(
        "跑一轮回测",
        &command_line(&["backtest", &runtime_text]),
        &retry,
        run_unified_backtest(Some(&runtime), None, None),
    );
    require(
        "读回摘要",
        &command_line(&["report", &runtime_text]),
        &retry,
        run_report(&runtime, false, false),
    );
    require(
        "看安全状态",
        &command_line(&["status", &runtime_text]),
        &retry,
        run_status(&runtime, false),
    );
    let (files, truncated) = project_file_count(&absolute, PROJECT_FILE_COUNT_BUDGET);
    println!();
    let counted = if truncated {
        format!(
            "项目里共 {files} 份文件（已在 {PROJECT_FILE_COUNT_BUDGET} 个目录的计数预算处截断，实际可能更多）"
        )
    } else {
        format!("项目里共 {files} 份文件")
    };
    println!("你刚做完了 5 步：建项目 → 静态检查 → 回测 → 读回摘要 → 安全状态，{counted}。");
    println!(
        "回测产物在 {} 下（summary / equity.csv / fills.csv / run.json 与数据集清单）。",
        absolute.join("data").display()
    );
    println!("下一步三条命令：");
    for step in [
        command_line(&["report", &runtime_text, "--json"]),
        command_line(&["strategy", "list"]),
        command_line(&[
            "init",
            absolute
                .join("qianxing.runtime.ashare.json")
                .to_string_lossy()
                .as_ref(),
            "--profile",
            "ashare",
        ]),
    ] {
        println!("  {step}");
    }
    // #261：这里同样不印注定报错的命令。paper-check 要 paper profile 那份带启用 Scheduler
    // worker 的运行时，对 macd 项目直接敲它以 2 退出，所以只在收尾句点名它的前置条件。
    println!(
        "本地 Paper 验收（{}）要先把运行时换成 paper profile 那份模板，否则会如实报「Paper 主链路缺少启用的 Scheduler worker」。",
        command_line(&["paper-check", &runtime_text])
    );
    println!(
        "全部入口：{}；这一行是哪个构建：{}",
        command_line(&["help"]),
        command_line(&["version"])
    );
}
