//! 首屏指引的可执行性（V11 Q54 第二批 / Q55）。
//!
//! `init` 与 `strategy init` 打印的"下一步"是用户对这套 CLI 的第一份文档：一条注定报错
//! 的命令比没有命令更糟。这里一律跑真 binary、照抄屏幕上的命令行，而不是在进程内调用
//! 同一批函数——会报错的是"命令面怎么把参数交给实现"这一层。

use super::*;
use std::process::Command;

/// 跑一次真 binary，返回（退出码, stdout+stderr 合并文本）。
fn qx_cli(args: &[&str]) -> (Option<i32>, String) {
    let output = Command::new(qx_cli_binary())
        .args(args)
        .output()
        .expect("启动 qx-cli 失败");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code(), text)
}

/// 从输出里取那条回测命令；profile 没有可跑的绑定时应当一行都不许出现。
fn advertised_backtest(text: &str) -> Option<String> {
    let line = text
        .lines()
        .find(|line| line.contains("qianxing backtest"))?;
    let start = line.find("qianxing backtest")?;
    Some(line[start..].trim_end().to_string())
}

/// 照屏幕上的样子把命令交给 binary：`qianxing` 只是提示符，参数按空白切开。
/// 临时目录带空格时切分会失真，所以先把它当成前置条件钉住。
fn run_as_printed(command: &str) -> (Option<i32>, String) {
    let args = command
        .split_whitespace()
        .skip(1)
        .inspect(|argument| {
            assert!(
                !argument.contains(' '),
                "参数含空白，切分会失真: {argument}"
            )
        })
        .collect::<Vec<_>>();
    qx_cli(&args)
}

/// 每个 profile 都跑：有 BarFrame 才许印回测命令，印出来的文件名必须真在项目里。
///
/// 反向验证：删掉 `init_backtest_step` 的 `?`（缺 BarFrame 时返回 None 的那一步），
/// `ccxt`/`multi-venue` 就会带着根本没复制的行情文件名重新印出命令，本用例的
/// `assert_eq!(advertised.is_some(), has_frame)` 与逐文件存在性当场不成立。
#[test]
fn advertised_commands_only_name_files_the_profile_generated() {
    for (profile, strategy) in [
        ("base", None),
        ("builtin", Some("macd")),
        ("paper", None),
        ("ccxt", None),
        ("ashare", None),
        ("multi-venue", None),
        ("backtest", None),
    ] {
        let root = temp_cli_case_dir(&format!("q55-onboard-{profile}"));
        let runtime = root.join("runtime.json").to_string_lossy().into_owned();
        let mut args = vec!["init", &runtime, "--profile", profile];
        if let Some(name) = strategy {
            args.push("--strategy");
            args.push(name);
        }
        let (code, stdout) = qx_cli(&args);
        assert_eq!(code, Some(0), "init --profile {profile} 失败:\n{stdout}");
        let readme = std::fs::read_to_string(root.join("README.qianxing.md"))
            .expect("init 必须生成 README.qianxing.md");
        let advertised = advertised_backtest(&stdout);
        assert_eq!(
            advertised.is_some(),
            readme.contains("qianxing backtest"),
            "profile={profile} 的首屏与 README 对不上: stdout={stdout}\n{readme}"
        );
        let has_frame = [
            "qianxing.bar-frame.example.json",
            "qianxing.ashare.bar-frame.example.json",
        ]
        .iter()
        .any(|name| root.join(name).is_file());
        assert_eq!(
            advertised.is_some(),
            has_frame,
            "profile={profile} 只在项目里有 BarFrame 时才该印回测命令: {advertised:?}"
        );
        for argument in advertised
            .iter()
            .flat_map(|command| command.split_whitespace())
        {
            if argument.ends_with(".json") {
                assert!(
                    Path::new(argument).is_file(),
                    "profile={profile} 印出的命令指向不存在的文件: {argument}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(root);
    }
}

/// 印出来的那几条命令照抄执行必须成功——"文件都在"不等于"入口读得动这份绑定"。
///
/// 覆盖四个不依赖外部解释器的 profile；`backtest` profile 绑的是跨语言策略，缺解释器时
/// 报的是解释器而不是路径，因此它的可跑性由 `e2e_and_python_contract` 一族负责。
///
/// 反向验证：把 `market_spec_with_margin` 换回只认 CCXT 形状，`builtin`/`ashare`/`base`
/// 三轮全部死在 "CCXT market 缺少 base"；把策略绑定判定改回恒真，`base`/`paper` 就会
/// 拿着没绑策略的运行时去跑 `backtest <runtime>` 而退出 2。
#[test]
fn advertised_backtest_commands_run_as_printed() {
    for (profile, strategy) in [
        ("base", None),
        ("builtin", Some("macd")),
        ("paper", None),
        ("ashare", None),
    ] {
        let root = temp_cli_case_dir(&format!("q55-run-{profile}"));
        let runtime = root.join("runtime.json").to_string_lossy().into_owned();
        let mut args = vec!["init", &runtime, "--profile", profile];
        if let Some(name) = strategy {
            args.push("--strategy");
            args.push(name);
        }
        let (_, stdout) = qx_cli(&args);
        let command = advertised_backtest(&stdout)
            .unwrap_or_else(|| panic!("profile={profile} 应当印出回测命令:\n{stdout}"));
        let (code, output) = run_as_printed(&command);
        assert_eq!(code, Some(0), "{command} 失败:\n{output}");
        assert!(
            output.contains("result_hash="),
            "{command} 退出 0 却没有给出结果指纹:\n{output}"
        );
        // 绑定运行时的入口会把摘要写进项目自己的 data/；`backtest builtin` 只打印结果，
        // 所以产物存在性只对印了路径的那一类 profile 判定。
        if let Some(summary) = output
            .lines()
            .find(|line| line.contains("summary="))
            .and_then(|line| line.split("summary=").nth(1))
            .and_then(|tail| tail.split_whitespace().next())
            .map(str::to_owned)
        {
            let normalize = |value: &str| value.replace('\\', "/");
            assert!(
                Path::new(&summary).is_file(),
                "{command} 声明的摘要并不存在: {summary}"
            );
            assert!(
                normalize(&summary).starts_with(&normalize(&root.to_string_lossy())),
                "产物写到了项目之外: {summary}"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }
}

/// `strategy init` 生成的配置必须自包含：屏幕上的第二条命令在任意 cwd 都能跑。
///
/// 反向验证：去掉 `run_strategy_init` 里的 `normalize_init_template_paths` 与资产复制，
/// 配置就会把 `deploy/qianxing.bar-frame.example.json` 写成相对配置文件目录的路径，
/// 打印的命令随即撞上 os error 3。
#[test]
fn strategy_init_writes_a_self_contained_project() {
    let root = temp_cli_case_dir("q55-strategy-init");
    let config = root.join("strategy.json");
    let (code, stdout) = qx_cli(&["strategy", "init", "sma_cross", &config.to_string_lossy()]);
    assert_eq!(code, Some(0), "strategy init 失败:\n{stdout}");
    let payload = std::fs::read_to_string(&config).unwrap();
    assert!(
        !payload.contains("deploy/"),
        "生成的策略配置仍指向仓库 deploy/ 目录，换到别处就找不到夹具:\n{payload}"
    );
    let step = stdout
        .lines()
        .find(|line| line.starts_with("下一步："))
        .unwrap_or_else(|| panic!("strategy init 没有给出下一步:\n{stdout}"));
    let command = step.trim_start_matches("下一步：");
    let (code, output) = run_as_printed(command);
    assert_eq!(code, Some(0), "{command} 失败:\n{output}");
    let _ = std::fs::remove_dir_all(root);
}

/// 双腿套利在两个单标的入口都要被拒，并且报出真正跑得动的那条路。
///
/// 反向验证：删掉 `single_leg_builtin_strategy` 的拒绝分支，`init --strategy
/// pairs_arbitrage` 会生成一份只在回测里报"内置策略参数非法"的配置，退出码变成 0。
#[test]
fn two_leg_builtin_strategies_are_refused_with_the_real_entry() {
    let root = temp_cli_case_dir("q55-two-leg");
    for kind in [
        "pairs_arbitrage",
        "basis_arbitrage",
        "cross_venue_arbitrage",
        "spot_futures_arbitrage",
    ] {
        let runtime = root.join(format!("init-{kind}.json"));
        let (code, stdout) = qx_cli(&["init", &runtime.to_string_lossy(), "--strategy", kind]);
        assert_eq!(code, Some(2), "init --strategy {kind} 必须拒绝:\n{stdout}");
        assert!(
            stdout.contains("multi-builtin") && stdout.contains("reference_instrument"),
            "init --strategy {kind} 没有说清哪条入口跑得动:\n{stdout}"
        );
        let strategy = root.join(format!("strategy-{kind}.json"));
        let (code, stdout) = qx_cli(&["strategy", "init", kind, &strategy.to_string_lossy()]);
        assert_eq!(code, Some(2), "strategy init {kind} 必须拒绝:\n{stdout}");
        assert!(
            stdout.contains("multi-builtin"),
            "strategy init {kind} 的报错没指路:\n{stdout}"
        );
        assert!(
            !runtime.exists() && !strategy.exists(),
            "拒绝后不许留下半份配置"
        );
    }
    // 报错文案点名的两份夹具必须真的存在，否则"改用 multi-builtin"又是第二条假指引。
    for name in [
        "qianxing.bar-frame.pairs-primary.example.json",
        "qianxing.bar-frame.pairs-reference.example.json",
    ] {
        assert!(
            repository_deploy_path(name).is_file(),
            "报错文案指向的示例夹具不存在: {name}"
        );
    }
    let (code, stdout) = qx_cli(&[
        "init",
        &root.join("single-leg.json").to_string_lossy(),
        "--strategy",
        "sma_cross",
    ]);
    assert_eq!(code, Some(0), "单标的策略仍须可用:\n{stdout}");
    let _ = std::fs::remove_dir_all(root);
}
