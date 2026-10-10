//! #204：`run <入口>` 不接受它读不懂的参数。
//!
//! `run_unified_command` 的 `backtest` 分支从 V12 R4-e 起就把多余旗标与位置参数报成用法
//! 错误，另外五条单配置文件入口却各自写了一遍"挑第一个非旗标参数，剩下的丢掉"：
//! `run doctor a.json b.json`、`run doctor --verbose` 都会照样跑完并以 0 退出。同一份
//! match 里两种相反的口径，运维照 help 写下的参数到底生不生效，只能靠猜。

use super::*;

const DEFAULT_PATH: &str = "default-runtime.json";

fn default_path() -> PathBuf {
    PathBuf::from(DEFAULT_PATH)
}

fn arguments(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

/// 能落地的参数形状只有三种：不给路径、给一个路径、给路径加 `--json`。
#[test]
fn run_entry_arguments_accept_only_the_shapes_they_can_honor() {
    assert_eq!(
        run_entry_arguments(&arguments(&["doctor"]), "doctor", true, default_path).unwrap(),
        (default_path(), false),
        "缺省路径要落到入口自己的默认值"
    );
    assert_eq!(
        run_entry_arguments(
            &arguments(&["doctor", "a.json", "--json"]),
            "doctor",
            true,
            default_path
        )
        .unwrap(),
        (PathBuf::from("a.json"), true),
        "路径与 --json 的合法组合必须原样读出"
    );
    assert!(
        run_entry_arguments(
            &arguments(&["doctor", "--json"]),
            "doctor",
            true,
            default_path
        )
        .unwrap()
        .1,
        "只给旗标时路径用默认值，但旗标本身要认账"
    );
}

/// 被丢掉的参数必须点名：只说"用法不对"等于让运维去猜哪一个旗标没生效。
/// 旗标给在路径之前与之后是两种形状——旧的"挑第一个非旗标参数"读法在两种形状上
/// 表现完全不同：给在后面时它还能靠"已经有一个路径"报出来，给在最前面时它会把
/// `--verbose` 当成配置文件路径默默接着跑。
#[test]
fn run_report_entry_arguments_accept_html_output_and_reject_ignored_paths() {
    let parsed = run_report_entry_arguments(
        &arguments(&[
            "report",
            "--html",
            "--output",
            "custom/report.html",
            "--json",
            "summary.json",
        ]),
        default_path,
    )
    .unwrap();
    assert_eq!(parsed.0, PathBuf::from("summary.json"));
    assert!(parsed.1 && parsed.2);
    assert!(!parsed.3, "没给 --evidence 时不得凭空生成证据包");
    assert_eq!(parsed.4, Some(PathBuf::from("custom/report.html")));
    let equals_form = run_report_entry_arguments(
        &arguments(&["report", "--html", "--output=chosen.html"]),
        default_path,
    )
    .unwrap();
    assert_eq!(equals_form.4, Some(PathBuf::from("chosen.html")));
    // T1-1：`--evidence` 是这条入口的第四个能力位，且和 `--json` 一样只许指定一次。
    let with_evidence = run_report_entry_arguments(
        &arguments(&["report", "--evidence", "summary.json"]),
        default_path,
    )
    .unwrap();
    assert_eq!(with_evidence.0, PathBuf::from("summary.json"));
    assert!(with_evidence.3, "--evidence 必须被读到");
    assert!(!with_evidence.2, "--evidence 不该顺手打开 --html");

    for given in [
        vec!["report", "--output", "report.html"],
        vec!["report", "--html", "--output"],
        vec!["report", "--html", "--output", "--json"],
        vec!["report", "--html", "--json", "--json"],
        vec!["report", "--html", "--html"],
        vec!["report", "--evidence", "--evidence"],
        vec!["report", "--html", "summary.json", "extra.json"],
    ] {
        let error = run_report_entry_arguments(&arguments(&given), default_path).unwrap_err();
        assert!(error.contains("run "), "拒绝形状必须给出入口用法: {error}");
    }
}

#[test]
fn unified_run_report_writes_html_and_standalone_svg_at_the_requested_stem() {
    let root = temp_cli_case_dir("run-report-output");
    let summary = root.join("sample.summary.json");
    let output = root.join("reports").join("chosen.html");
    std::fs::write(&summary, "{\"schema_version\":1}").unwrap();
    let argv = vec![
        "report".to_string(),
        summary.to_string_lossy().into_owned(),
        "--json".to_string(),
        "--html".to_string(),
        "--output".to_string(),
        output.to_string_lossy().into_owned(),
    ];
    run_unified_command(&argv).expect("run report --html 应走同一个摘要复核与导出路径");
    assert!(output.is_file());
    for suffix in ["equity.svg", "fills.svg", "monthly.svg"] {
        let path = root.join("reports").join(format!("chosen.{suffix}"));
        assert!(path.is_file(), "缺少统一入口输出 {}", path.display());
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn run_entry_arguments_name_every_argument_they_reject() {
    for (entry, json_output, given, rejected) in [
        (
            "doctor",
            true,
            ["doctor", DEFAULT_PATH, "b.json"].to_vec(),
            "b.json",
        ),
        (
            "doctor",
            true,
            ["doctor", DEFAULT_PATH, "--verbose"].to_vec(),
            "--verbose",
        ),
        (
            "doctor",
            true,
            ["doctor", "--verbose"].to_vec(),
            "--verbose",
        ),
        (
            "report",
            true,
            ["report", "--format=text"].to_vec(),
            "--format=text",
        ),
        ("paper", false, ["paper", "--json"].to_vec(), "--json"),
        (
            "paper",
            false,
            ["paper", "x.json", "--json"].to_vec(),
            "--json",
        ),
    ] {
        let error = run_entry_arguments(&arguments(&given), entry, json_output, default_path)
            .expect_err(&format!("{entry} 不该收下 {rejected}"));
        assert!(
            error.contains(rejected) && error.contains("run "),
            "拒绝文案要点名被丢下的参数并给出用法: {error}"
        );
    }
}

/// 派发侧接线：五条单配置文件入口都得走同一份读法，且旧的"丢掉其余参数"读法不得残留。
/// 光测纯函数不会发现某条臂又退回自己挑参数（那正是 #204 的原始形状）。
#[test]
fn every_single_path_run_entry_shares_the_argument_reader() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let source = std::fs::read_to_string(
        root.join("crates")
            .join("qx-cli")
            .join("src")
            .join("config_commands.rs"),
    )
    .unwrap();
    assert_eq!(
        source.matches("run_entry_arguments(").count(),
        4,
        "四条通用单配置入口不是都共用同一份参数读法"
    );
    assert_eq!(
        source.matches("run_report_entry_arguments(").count(),
        1,
        "report 必须通过专用 reader 同时处理 --html 与 --output"
    );
    assert!(
        !source.contains(".find(|value| !value.starts_with('-'))"),
        "「挑第一个非旗标参数、剩下的丢掉」的读法回来了"
    );
    assert!(
        !source.contains("argument == \"--json\""),
        "入口各自找 --json：旗标是否生效又变成逐臂的运气"
    );
}
