//! 默认配置路径的查找面（V13 第三十一遍 ②，易用性 F1–F6）。
//!
//! 判据全部取自本轮实测（`logs/s738_pass31_lookup_surface_probe.txt`），不是设计愿望。改前同一棵
//! 树、同一个无关启动目录：`doctor`/`status` 退 0，而 `runtime-check`/`live-check`/`paper-check`
//! 退 2 并回 `读取运行时配置失败 deploy/…`（`logs/s734_pass31_default_path_probe.txt`）——因为
//! 后一组把 `"deploy/…"` 字面量拼在当前目录上，前一组经 `repository_deploy_path` 回仓库。现在
//! 两条路合成一条链，这一份文件钉的就是"从别处启动也只有一个口径"。
//!
//! 单元侧（`src/tests/deploy_lookup.rs`）判链条形状；这里判屏幕与退出码。两边不重复：链条顺序
//! 在子进程里没法证伪，端到端"能不能跑通"在进程内又拿不到真 stderr。

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// 临时目录按用例名与进程号取：集成用例并行跑，共用一个目录会互相踩产物。
fn temp_base(label: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("qianxing-lookup-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("创建临时运行目录失败");
    base
}

/// 从 `cwd` 启动真 binary，并且剥掉两个会改变查找结果的环境变量：
/// `QX_DEPLOY_DIR` 是本链的第一优先级，`QX_PYTHON` 决定裸 `backtest` 走不走解释器。
fn run(cwd: &Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(binary())
        .current_dir(cwd)
        .env_remove("QX_DEPLOY_DIR")
        .env_remove("QX_PYTHON")
        .args(args)
        .output()
        .expect("启动 qx-cli 失败");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// 只读入口在无关目录里一律跑得通：改前三条退 2，这一条判据就是那次实测的反面。
#[test]
fn readonly_entries_run_from_an_unrelated_directory() {
    let base = temp_base("readonly");
    let expectations = [
        ("doctor", "[Doctor] 通过"),
        ("status", "[Status] runtime="),
        ("runtime-check", "[PASS] runtime 引用文件校验通过"),
        ("paper-check", "[Paper · E2E]"),
    ];
    for (entry, token) in expectations {
        let (code, stdout, stderr) = run(&base, &[entry]);
        assert_eq!(code, 0, "{entry} 在无关目录里必须退 0:\n{stderr}");
        assert!(
            stdout.contains(token),
            "{entry} 跑通了却没印出本轮实测的那句关键输出:\n{stdout}"
        );
        assert!(
            !stderr.contains("读取运行时配置失败"),
            "{entry} 又在当前目录里找默认配置:\n{stderr}"
        );
    }
    let _ = std::fs::remove_dir_all(base);
}

/// 裸 `backtest` 不需要任何环境变量：改前默认那份要 Python worker（本机 `python` 是占位桩，
/// 退 2），与 README 的"装完就能跑一条回测"直接矛盾，所以默认换成了内置策略模板（F6）。
/// 屏幕上那句"产物写在该配置的 storage.data_dir，相对当前目录解析"也得是真的。
#[test]
fn bare_backtest_needs_no_interpreter_and_lands_where_it_says() {
    let base = temp_base("backtest");
    let (code, stdout, stderr) = run(&base, &["backtest"]);
    assert_eq!(code, 0, "不带解释器环境的裸回测失败:\n{stderr}");
    assert!(
        stdout.contains("[默认输入] 运行时配置="),
        "裸回测必须说出用的是哪份默认配置:\n{stdout}"
    );
    assert!(
        stdout.contains("[默认落点] 产物写在该配置的 storage.data_dir，相对当前目录"),
        "裸回测必须交代产物落点按哪个目录解析:\n{stdout}"
    );
    assert!(
        stdout.contains("result_hash=") && stdout.contains("fills="),
        "裸回测必须真跑出成交与结果指纹:\n{stdout}"
    );
    for token in stdout.split_whitespace() {
        let Some(path) = token.strip_prefix("path=") else {
            continue;
        };
        assert!(
            base.join(path).is_file(),
            "[RunManifest] 声明的落点在启动目录里不存在: {path}"
        );
    }
    let _ = std::fs::remove_dir_all(base);
}

/// 用法回显里出现的必须是这台机器上敲得动的程序名，而不是构建产物的文件名（F3）。
#[test]
fn usage_echo_names_the_program_not_the_build_artifact() {
    let base = temp_base("usage");
    let (code, _, stderr) = run(&base, &["nope"]);
    assert_eq!(code, 2, "未知子命令必须以 2 退出:\n{stderr}");
    assert!(
        stderr.contains("Usage: qx-cli "),
        "用法回显没有以程序名开头:\n{stderr}"
    );
    assert!(
        !stderr.contains(".exe"),
        "用法回显漏出了构建产物的扩展名:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(base);
}

/// 走到别处那一份时必须说出来：屏幕上那行 `config_fingerprint=` 从此描述的是另一份文件。
/// 同一个路径在仓库根里本来就有，此时一个字都不许多印——否则这条判据无法区分"定位到了"与
/// "无条件念稿"。
#[test]
fn a_walked_example_shape_says_so_and_a_local_one_does_not() {
    let typed = "deploy/qianxing.runtime.example.json";
    let base = temp_base("announced");
    let (code, _, stderr) = run(&base, &["runtime-check", typed]);
    assert_eq!(code, 0, "示例形状在别处存在时该跑通:\n{stderr}");
    assert!(
        stderr.contains("[查找 · Lookup]") && stderr.contains(typed) && stderr.contains("改用"),
        "换了位置却没说出来:\n{stderr}"
    );
    let root = repository_root();
    assert!(
        root.join(typed).is_file(),
        "判据依赖仓库根里的那一份: {}",
        root.join(typed).display()
    );
    let (code, _, stderr) = run(&root, &["runtime-check", typed]);
    assert_eq!(code, 0, "在仓库根里逐字给路径必须跑得通:\n{stderr}");
    assert!(
        !stderr.contains("[查找 · Lookup]"),
        "当前目录本来就有这一份，不该声称换过位置:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(base);
}

/// A 股快速回测的入口也走这条链（① 尾 #271）。改前在无关目录里逐字敲文档那条命令，只回一行
/// `读取快速回测 manifest 失败 …: 系统找不到指定的路径。 (os error 3)`，一个字都没提"这一份
/// 其实就在这台机器上"（`logs/s750_pass31_standalone_fast_backtest.txt`）。现在它退 0、说出换到
/// 了哪里，并且把 manifest 里 jobs 按同级文件名引用的 runtime/bars/spec 一起读齐——最后一格判的
/// 是内置层"一次落整份清单"那条规则：只落 manifest 本身，作业就会在下一格读取上断掉。
#[test]
fn the_ashare_fast_backtest_entry_runs_from_an_unrelated_directory() {
    let base = temp_base("fast-backtest");
    let typed = "deploy/qianxing.fast-backtest.ashare.example.json";
    let (code, stdout, stderr) = run(&base, &["fast-backtest", typed]);
    assert_eq!(
        code, 0,
        "A 股快速回测在无关目录里必须退 0:\n{stdout}\n{stderr}"
    );
    assert!(
        stderr.contains("[查找 · Lookup]") && stderr.contains(typed),
        "manifest 换了位置却没说出来:\n{stderr}"
    );
    assert!(
        stdout.contains("jobs=1 completed=1") && stdout.contains("fills=1"),
        "那份 manifest 的一个作业必须真跑出成交:\n{stdout}"
    );
    assert!(
        stdout.contains("result_hash="),
        "快速回测要给出可复现的结果指纹:\n{stdout}"
    );
    for token in stdout.split_whitespace() {
        let Some(path) = token.strip_prefix("path=") else {
            continue;
        };
        assert!(
            base.join(path).is_file(),
            "[RunManifest] 声明的落点在启动目录里不存在: {path}"
        );
    }
    let _ = std::fs::remove_dir_all(base);
}

/// 查找面过去只接了一半只读入口（#274）：带 `default_value` 的那族会搬迁，而
/// `backtest`/`paper-submit-order`/`reconcile` 这三个「读取示例输入」的位置参数没有默认值，
/// 手打的同一条 `deploy/…` 只按当前目录解析。改前在无关目录里逐字敲这三条，只回一行
/// `读取运行时配置失败 …: 系统找不到指定的路径。 (os error 3)`（`logs/s774_pass32_lookup_unmounted_before.txt`）。
/// 现在三者都挂上了同一个解析器：换位置要说出 `[查找 · Lookup]`，且报错里绝不剩 os error 3。
/// 只判「找到没找到」，不锁下游退出码——`backtest` 用这份示例会继续走到跨语言缺口、
/// `reconcile` 会走到「没有 reconciler worker」，那些是与查找面无关的正确失败。
#[test]
fn the_read_input_positionals_relocate_from_an_unrelated_directory() {
    let base = temp_base("mounted-positionals");
    let runtime = "deploy/qianxing.runtime.paper-strategy.example.json";
    let command = "deploy/qianxing.paper-submit-order.example.json";

    let (_, _, stderr) = run(&base, &["backtest", runtime]);
    assert!(
        stderr.contains("[查找 · Lookup]") && stderr.contains(runtime) && stderr.contains("改用"),
        "backtest 的 runtime 位置参数换了位置却没说出来:\n{stderr}"
    );
    assert!(
        !stderr.contains("os error 3") && !stderr.contains("读取运行时配置失败"),
        "backtest 仍在当前目录找那份 runtime:\n{stderr}"
    );

    let (_, _, stderr) = run(&base, &["reconcile", runtime]);
    assert!(
        stderr.contains("[查找 · Lookup]") && stderr.contains(runtime) && stderr.contains("改用"),
        "reconcile 的 runtime 位置参数换了位置却没说出来:\n{stderr}"
    );
    assert!(
        !stderr.contains("os error 3") && !stderr.contains("读取运行时配置失败"),
        "reconcile 仍在当前目录找那份 runtime:\n{stderr}"
    );

    // paper-submit-order 两份输入都挂了解析器：runtime 与命令都在别处，逐条点名后应当被读到。
    // 这里判的是「找没找到」而不是「成交没成交」——干净目录里没有 BTCUSDT 的新鲜行情，
    // 提交会按 #273 记成终态失败（FAIL_CLOSED 缺行情），但那句失败恰恰证明两份输入都已加载。
    let (_, _, stderr) = run(&base, &["paper-submit-order", runtime, command]);
    assert!(
        stderr.matches("[查找 · Lookup]").count() == 2,
        "runtime 与 command 两份都要各说一句查找:\n{stderr}"
    );
    assert!(
        stderr.contains("Paper SubmitOrder"),
        "两份输入都读到之后应当走到提交通道:\n{stderr}"
    );
    assert!(
        !stderr.contains("os error 3") && !stderr.contains("读取运行时配置失败"),
        "paper-submit-order 仍在当前目录找那份 runtime:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(base);
}

/// worker 入口拿的是精确路径：不换位置，但要把"别处真有这一份"点名（F2 的另一半）。
/// 这一条同时是 `deploy_relocation_hint` 的生产读者——没有它，那段补话就是死代码。
#[test]
fn worker_entries_take_the_path_verbatim_and_point_at_the_other_copy() {
    let base = temp_base("worker");
    let typed = "deploy/qianxing.runtime.example.json";
    let (code, _, stderr) = run(&base, &["scheduler-worker", typed, "scheduler", "--once"]);
    assert_eq!(code, 2, "worker 入口把路径换掉才算跑偏:\n{stderr}");
    assert!(
        stderr.contains(&format!("读取运行时配置失败 {typed}")),
        "worker 入口必须按使用者写下的那条路径报错:\n{stderr}"
    );
    assert!(
        stderr.contains("这一份示例在别处存在"),
        "别处真有这一份时要点名，而不是把使用者留在 os error 3 前:\n{stderr}"
    );
    assert!(
        stderr.contains(typed)
            && stderr.contains(&repository_root().join("deploy").display().to_string()),
        "补话要点名那一份的完整位置:\n{stderr}"
    );
    let _ = std::fs::remove_dir_all(base);
}

/// `paper-check` 在同一份运行目录里连跑两遍（V13 R2 #275）。改前第二遍照旧印
/// `[Paper · E2E] … orders=1 ledger_entries=4 ✓` 并退 0，可那一轮的订单与账本全是
/// 第一遍留下的既有事实——本轮调度 `skipped=1`、策略与执行各 `processed=0`，端到端一手
/// 没跑（`logs/s782_pass32_paper_check_doublerun.txt`）。空转被报成验收通过，就是
/// 「日志里有 ✓ 但没人敲第二遍」那一类。现在末行按「本轮新增」给结论：只有真跑出
/// 成交的一遍带 ✓，同日第二遍如实说「本轮零新增」且不许出现 ✓。两遍都必须退 0——
/// 当日调度按天平幂等是设计属性，空转是合法 no-op，不是失败（`e2e_and_python_contract.rs`
/// 对连跑两遍 `.unwrap()`，这条判据不能把它打红）。
#[test]
fn paper_check_same_day_rerun_does_not_claim_a_fresh_success() {
    let base = temp_base("paper-doublerun");

    let (code, stdout, stderr) = run(&base, &["paper-check"]);
    assert_eq!(code, 0, "首跑 Paper 主链路必须退 0:\n{stdout}\n{stderr}");
    assert!(
        stdout.contains("[Paper · E2E]")
            && stdout.contains("orders=1 (+1 本轮新增)")
            && stdout.contains('✓'),
        "首跑真跑出一手成交时末行要写出 orders +1 并以 ✓ 收尾:\n{stdout}"
    );

    let (code, stdout, stderr) = run(&base, &["paper-check"]);
    assert_eq!(
        code, 0,
        "同日重跑是合法 no-op，仍要退 0:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("[Paper · E2E]") && stdout.contains("本轮零新增"),
        "同日第二遍要说清本轮没端到端重跑:\n{stdout}"
    );
    assert!(
        !stdout.contains('✓'),
        "本轮零新增的一遍不许再打验收通过的成功符:\n{stdout}"
    );
    assert!(
        stdout.contains("orders=1 (+0 本轮新增)"),
        "复用上一轮事实时累计数要与本轮 +0 一并写出:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(base);
}
