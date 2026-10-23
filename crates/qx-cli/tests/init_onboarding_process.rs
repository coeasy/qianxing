//! First-run guidance is exercised by copying and running the commands a user sees.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli integration tests are two levels below the workspace root")
        .to_path_buf()
}

fn temp_case_dir(label: &str) -> PathBuf {
    let root = workspace_root()
        .join("target/qx-cli-test-tmp")
        .join(format!(
            "qianxing-cli-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn qx_cli(args: &[&str]) -> (Option<i32>, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(workspace_root())
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run qx-cli {args:?}: {error}"));
    (
        output.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

fn advertised_backtest(text: &str) -> Option<String> {
    let needle = format!("{} backtest", env!("CARGO_PKG_NAME"));
    let line = text.lines().find(|line| line.contains(&needle))?;
    let start = line.find(&needle)?;
    Some(line[start..].trim_end().to_string())
}

fn run_as_printed(command: &str) -> (Option<i32>, String) {
    let args = command
        .split_whitespace()
        .skip(1)
        .inspect(|argument| {
            assert!(
                !argument.contains(' '),
                "printed argument contains spaces and cannot be copied as documented: {argument}"
            )
        })
        .collect::<Vec<_>>();
    qx_cli(&args)
}

fn init_args<'a>(runtime: &'a str, profile: &'a str, strategy: Option<&'a str>) -> Vec<&'a str> {
    let mut args = vec!["init", runtime, "--profile", profile];
    if let Some(strategy) = strategy {
        args.extend(["--strategy", strategy]);
    }
    args
}

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
        let root = temp_case_dir(&format!("q55-onboard-{profile}"));
        let runtime = root.join("runtime.json").to_string_lossy().into_owned();
        let args = init_args(&runtime, profile, strategy);
        let (code, stdout) = qx_cli(&args);
        assert_eq!(code, Some(0), "init --profile {profile} failed:\n{stdout}");
        let readme = std::fs::read_to_string(root.join("README.qianxing.md"))
            .expect("init should generate README.qianxing.md");
        let advertised = advertised_backtest(&stdout);
        assert_eq!(
            advertised.is_some(),
            readme.contains(&format!("{} backtest", env!("CARGO_PKG_NAME"))),
            "profile={profile} first-run output and README disagree: stdout={stdout}\n{readme}"
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
            "profile={profile} may advertise backtest only when it generated a BarFrame: {advertised:?}"
        );
        for argument in advertised
            .iter()
            .flat_map(|command| command.split_whitespace())
        {
            if argument.ends_with(".json") {
                assert!(
                    Path::new(argument).is_file(),
                    "profile={profile} advertises a missing file: {argument}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn advertised_backtest_commands_run_as_printed() {
    for (profile, strategy) in [
        ("base", None),
        ("builtin", Some("macd")),
        ("paper", None),
        ("ashare", None),
    ] {
        let root = temp_case_dir(&format!("q55-run-{profile}"));
        let runtime = root.join("runtime.json").to_string_lossy().into_owned();
        let args = init_args(&runtime, profile, strategy);
        let (_, stdout) = qx_cli(&args);
        let command = advertised_backtest(&stdout)
            .unwrap_or_else(|| panic!("profile={profile} should advertise backtest:\n{stdout}"));
        let (code, output) = run_as_printed(&command);
        assert_eq!(code, Some(0), "{command} failed:\n{output}");
        assert!(
            output.contains("result_hash="),
            "{command} exited successfully without a result hash:\n{output}"
        );
        if let Some(summary) = output
            .lines()
            .find(|line| line.contains("summary="))
            .and_then(|line| line.split("summary=").nth(1))
            .and_then(|tail| tail.split_whitespace().next())
        {
            let normalize = |value: &str| value.replace('\\', "/");
            assert!(
                Path::new(summary).is_file(),
                "reported summary is missing: {summary}"
            );
            assert!(
                normalize(summary).starts_with(&normalize(&root.to_string_lossy())),
                "backtest artifact escaped the generated project: {summary}"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn readme_recommended_flow_runs_line_by_line() {
    let program = env!("CARGO_PKG_NAME");
    for (profile, strategy) in [
        ("base", None),
        ("builtin", Some("macd")),
        ("paper", None),
        ("ashare", None),
    ] {
        let root = temp_case_dir(&format!("u4-readme-flow-{profile}"));
        let runtime = root.join("runtime.json").to_string_lossy().into_owned();
        let args = init_args(&runtime, profile, strategy);
        let (code, stdout) = qx_cli(&args);
        assert_eq!(code, Some(0), "init --profile {profile} failed:\n{stdout}");
        let readme = std::fs::read_to_string(root.join("README.qianxing.md"))
            .expect("init should generate README.qianxing.md");
        let flow = readme
            .split("```text\n")
            .nth(1)
            .and_then(|tail| tail.split("\n```").next())
            .unwrap_or_else(|| panic!("profile={profile} README has no command block:\n{readme}"));
        let commands = flow
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with(&format!("{program} ")))
            .collect::<Vec<_>>();
        assert!(
            !commands.is_empty(),
            "profile={profile} README flow is empty:\n{flow}"
        );
        for line in &commands {
            let (code, output) = run_as_printed(line);
            assert_eq!(
                code,
                Some(0),
                "profile={profile} README command failed: {line}\n{output}"
            );
        }

        let backtest_prefix = format!("{program} backtest ");
        let bound_backtest = commands.iter().any(|line| {
            line.strip_prefix(backtest_prefix.as_str())
                .is_some_and(|arguments| {
                    arguments.split_whitespace().next() == Some(runtime.as_str())
                })
        });
        let tail = *commands
            .last()
            .expect("README flow has at least one command");
        if profile == "paper" {
            assert!(
                tail.starts_with(&format!("{program} paper-check")),
                "{tail}"
            );
        } else if bound_backtest {
            assert!(!tail.contains("paper-check"), "{tail}");
            assert!(
                tail.starts_with(&format!("{program} report")) && tail.ends_with("--json"),
                "backtest projects should finish by reading the summary: {tail}"
            );
        } else {
            assert!(tail.starts_with(&format!("{program} backtest")), "{tail}");
        }
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn strategy_init_writes_a_self_contained_project() {
    let root = temp_case_dir("q55-strategy-init");
    let config = root.join("strategy.json");
    let (code, stdout) = qx_cli(&["strategy", "init", "sma_cross", &config.to_string_lossy()]);
    assert_eq!(code, Some(0), "strategy init failed:\n{stdout}");
    let payload = std::fs::read_to_string(&config).unwrap();
    assert!(
        !payload.contains("deploy/"),
        "generated config leaks deploy paths:\n{payload}"
    );
    let step = stdout
        .lines()
        .find(|line| line.starts_with("下一步："))
        .unwrap_or_else(|| panic!("strategy init has no next step:\n{stdout}"));
    let (code, output) = run_as_printed(step.trim_start_matches("下一步："));
    assert_eq!(code, Some(0), "strategy init next step failed:\n{output}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn two_leg_builtin_strategies_are_refused_with_the_real_entry() {
    let root = temp_case_dir("q55-two-leg");
    for kind in [
        "pairs_arbitrage",
        "basis_arbitrage",
        "cross_venue_arbitrage",
        "spot_futures_arbitrage",
    ] {
        let runtime = root.join(format!("init-{kind}.json"));
        let (code, stdout) = qx_cli(&["init", &runtime.to_string_lossy(), "--strategy", kind]);
        assert_eq!(code, Some(2), "init --strategy {kind} must fail:\n{stdout}");
        assert!(
            stdout.contains("multi-builtin") && stdout.contains("reference_instrument"),
            "init --strategy {kind} must point to the working route:\n{stdout}"
        );
        let strategy = root.join(format!("strategy-{kind}.json"));
        let (code, stdout) = qx_cli(&["strategy", "init", kind, &strategy.to_string_lossy()]);
        assert_eq!(code, Some(2), "strategy init {kind} must fail:\n{stdout}");
        assert!(stdout.contains("multi-builtin"), "{stdout}");
        assert!(
            !runtime.exists() && !strategy.exists(),
            "rejection left a partial config"
        );
    }
    let deploy = workspace_root().join("deploy");
    for name in [
        "qianxing.bar-frame.pairs-primary.example.json",
        "qianxing.bar-frame.pairs-reference.example.json",
    ] {
        assert!(
            deploy.join(name).is_file(),
            "referenced fixture is missing: {name}"
        );
    }
    let runtime = root.join("single-leg.json");
    let (code, stdout) = qx_cli(&[
        "init",
        &runtime.to_string_lossy(),
        "--strategy",
        "sma_cross",
    ]);
    assert_eq!(
        code,
        Some(0),
        "single-leg strategy must remain available:\n{stdout}"
    );
    let _ = std::fs::remove_dir_all(root);
}

fn count_files(dir: &Path) -> usize {
    let mut stack = vec![dir.to_path_buf()];
    let mut files = 0;
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(current).expect("directory should be readable") {
            let path = entry.expect("directory entry should be readable").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files += 1;
            }
        }
    }
    files
}

#[test]
fn init_lands_ten_files_and_the_advertised_backtest_adds_six() {
    let root = temp_case_dir("r2-init-file-count");
    let runtime = root.join("qianxing.runtime.json");
    let (code, stdout) = qx_cli(&[
        "init",
        &runtime.to_string_lossy(),
        "--profile",
        "builtin",
        "--strategy",
        "macd",
    ]);
    assert_eq!(code, Some(0), "init failed:\n{stdout}");
    assert_eq!(count_files(&root), 10, "init project asset count changed");
    let command = advertised_backtest(&stdout).expect("builtin profile should advertise backtest");
    let (code, output) = run_as_printed(&command);
    assert_eq!(code, Some(0), "{command} failed:\n{output}");
    assert_eq!(count_files(&root), 16, "backtest artifact count changed");
    assert!(
        root.join("data/qianxing/datasets.manifest.json").is_file(),
        "dataset manifest was not written"
    );
    let runs = std::fs::read_dir(root.join("data/qianxing/runs"))
        .expect("runs directory should exist")
        .count();
    assert_eq!(
        runs, 5,
        "one run should write four core files and one event-trace sidecar"
    );
    let _ = std::fs::remove_dir_all(root);
}
