//! 结果可读性层（易用性 P3）的行为用例：HTML 报告必须**确定性**、**无外部资源**、
//! 且**不画出假的成交点**。这三条是本层最容易悄悄退化的性质：确定性靠「同输入两次逐字节
//! 相等」钉，无外链靠「出现 `http` 即红」钉，成交点靠「0 成交不画三角」钉。

use super::*;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("qx-report-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 渲染入口的用例侧薄封装：月度收益走生产同一条算式入口 [`monthly_series`]，
/// 不让用例自带第二份月度收益算法（否则用例与产品各算一套，漂移时两边都「自洽」）。
fn render(summary: &serde_json::Value, equity: &[(i64, i128)], fills: &[FillPoint]) -> String {
    render_report_html(summary, equity, fills, &monthly_series(summary, equity))
}

/// 一份形状完整的 v4 摘要：只有下面这些格子，够渲染全部卡片与身份行。
fn sample_summary() -> serde_json::Value {
    serde_json::json!({
        "schema_version": 4,
        "strategy_id": "strategy-macd",
        "instrument": "BTCUSDT.BINANCE",
        "input": {
            "kind": "barframe",
            "path": "/tmp/bars.json",
            "dataset_id": "strategy-bars:BTCUSDT.BINANCE",
            "dataset_version": "fixture-dataset-v1",
            "fingerprint": "abc123"
        },
        "account": { "initial_cash_raw": "100000000000000", "source": "builtin-default" },
        "fills": 1,
        "rejected_orders": 0,
        "input_data_hash": "deadbeef",
        "result_hash": "feedface",
        "metrics": {
            "return_bps": 125,
            "max_drawdown_bps": 40,
            "fees_raw": 43250000,
            "turnover_raw": 86_500_000_000_i64,
            "final_equity_raw": 100125000000000_i64,
            "sharpe": 1.25,
            "sortino": 1.75,
            "calmar": 3.125,
            "win_rate": 0.67,
            "profit_factor": 1.9
        },
        "matching_kernel": "qx-xingban::BacktestEngine(bar)",
        "risk_rules": { "rule_set_version": "v1", "source": "runtime-config" },
        "execution_costs": { "source": "runtime-config-default" },
        "replay": { "log_digest": "feedface", "events": 7, "ledger_entries": 4, "run_ledger_entries": 4 },
        "run_manifest": "/tmp/x.run.json"
    })
}

fn sample_equity() -> Vec<(i64, i128)> {
    vec![
        (1_700_000_000_000, 100_000_000_000_000),
        (1_700_086_400_000, 100_050_000_000_000),
        (1_700_172_800_000, 100_125_000_000_000),
    ]
}

fn sample_fills() -> Vec<FillPoint> {
    vec![FillPoint {
        ts: 1_700_086_400_000,
        qty_raw: 1_000_000_000,
        price_raw: 86_500_000_000,
    }]
}

#[test]
fn report_output_flag_is_part_of_direct_cli_surface() {
    use clap::Parser;
    let parsed = cli_args::Cli::try_parse_from([
        "qx-cli",
        "report",
        "summary.json",
        "--html",
        "-o",
        "custom.html",
    ])
    .unwrap();
    match parsed.command.unwrap() {
        cli_args::Command::Report {
            path, html, out, ..
        } => {
            assert_eq!(path, Some(PathBuf::from("summary.json")));
            assert!(html);
            assert_eq!(out, Some(PathBuf::from("custom.html")));
        }
        _ => panic!("report 参数必须落在 Report 命令变体"),
    }
}

#[test]
fn json_mode_with_html_keeps_stdout_parseable_and_lists_all_outputs() {
    let dir = temp_dir("json-output");
    let summary = dir.join("run.summary.json");
    let html = dir.join("reports").join("chosen.html");
    std::fs::write(&summary, "{\"schema_version\":1}").unwrap();
    let json_only = std::process::Command::new(qx_cli_binary())
        .arg("report")
        .arg(&summary)
        .arg("--json")
        .output()
        .unwrap();
    assert!(json_only.status.success());
    let plain_report: serde_json::Value = serde_json::from_slice(&json_only.stdout).unwrap();
    assert!(plain_report.get("generated_artifacts").is_none());
    let output = std::process::Command::new(qx_cli_binary())
        .arg("report")
        .arg(&summary)
        .arg("--json")
        .arg("--html")
        .arg("-o")
        .arg(&html)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "report --json --html 应成功: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["generated_artifacts"]["html"],
        html.display().to_string()
    );
    for key in ["equity_svg", "fills_svg", "monthly_svg"] {
        let path = PathBuf::from(report["generated_artifacts"][key].as_str().unwrap());
        assert!(
            path.is_file(),
            "JSON 声明的图表必须真实落盘: {}",
            path.display()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn report_is_byte_equal_across_runs() {
    let summary = sample_summary();
    let first = render(&summary, &sample_equity(), &sample_fills());
    let second = render(&summary, &sample_equity(), &sample_fills());
    assert_eq!(first, second, "同输入两次渲染必须逐字节相等");
    assert!(first.starts_with("<!DOCTYPE html>"));
    assert!(first.contains("牵星回测报告"));
}

#[test]
fn report_has_no_external_resource_reference() {
    let html = render(&sample_summary(), &sample_equity(), &sample_fills());
    // 内联 SVG 不带 `xmlns`、样式全内联：整篇不出现 http/https，就没有意外外链。
    assert!(!html.contains("http"), "报告不得出现任何 http 引用");
    assert!(!html.contains("src="), "报告不得引入外部脚本/图片");
    assert!(html.contains("<svg"), "三张图必须内嵌");
}

#[test]
fn zero_fills_draw_no_marker_and_each_fill_adds_one() {
    let summary = sample_summary();
    let none = render(&summary, &sample_equity(), &[]);
    assert_eq!(none.matches("<polygon").count(), 0, "0 成交不得画买卖点");
    assert!(
        none.contains("无成交数据"),
        "0 成交必须显式说明，而不是空白"
    );

    let two = vec![
        FillPoint {
            ts: 1,
            qty_raw: 1_000_000_000,
            price_raw: 86_500_000_000,
        },
        FillPoint {
            ts: 2,
            qty_raw: -1_000_000_000,
            price_raw: 87_000_000_000,
        },
    ];
    let both = render(&summary, &sample_equity(), &two);
    assert_eq!(both.matches("<polygon").count(), 2, "每笔成交一个标记");
    // 买红卖绿（中国市场口径）。
    assert!(both.contains("#c0392b") && both.contains("#1e8449"));
}

#[test]
fn cards_read_from_summary_fields_only() {
    let html = render(&sample_summary(), &sample_equity(), &sample_fills());
    // 收益率 125 bps → +1.25%；回撤 40 bps → 0.40%；期末权益与手续费按 1e9 刻度折算。
    assert!(html.contains("+1.25%"), "收益率卡应等于 metrics.return_bps");
    assert!(
        html.contains("0.40%"),
        "回撤卡应等于 metrics.max_drawdown_bps"
    );
    assert!(
        html.contains("¥100125.00"),
        "期末权益应等于 metrics.final_equity_raw"
    );
    assert!(
        html.contains("¥100000.00"),
        "期初本金应等于 account.initial_cash_raw"
    );
    assert!(html.contains("¥0.04"), "手续费应等于 metrics.fees_raw");
    for displayed in [">1.25<", ">1.75<", ">3.12<", ">0.67<", ">1.90<"] {
        assert!(
            html.contains(displayed),
            "风险比率卡必须读取 summary.metrics: {displayed}"
        );
    }
}

#[test]
fn write_report_html_lands_next_to_summary_and_rejects_bad_csv() {
    let dir = temp_dir("write");
    let summary_path = dir.join("run.summary.json");
    std::fs::write(&summary_path, "{}").unwrap();
    std::fs::write(
        dir.join("run.equity.csv"),
        "index,ts,equity_raw,position_raw\n0,1000,100000000000000,0\n1,2000,100010000000000,0\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("run.fills.csv"),
        "order_id,ts,qty_raw,price_raw\n1,1500,1000000000,86500000000\n",
    )
    .unwrap();
    let out = write_report_html(&summary_path, &sample_summary(), None).unwrap();
    assert_eq!(
        out.html.file_name().unwrap().to_str().unwrap(),
        "run.report.html"
    );
    assert_eq!(out.equity_svg.file_name().unwrap(), "run.equity.svg");
    assert_eq!(out.fills_svg.file_name().unwrap(), "run.fills.svg");
    assert_eq!(out.monthly_svg.file_name().unwrap(), "run.monthly.svg");
    assert!(out.equity_svg.is_file() && out.fills_svg.is_file() && out.monthly_svg.is_file());
    for svg in [&out.equity_svg, &out.fills_svg, &out.monthly_svg] {
        let payload = std::fs::read_to_string(svg).unwrap();
        assert!(
            payload.starts_with("<svg") && !payload.contains("http") && !payload.contains("xmlns")
        );
    }
    let html = std::fs::read_to_string(&out.html).unwrap();
    assert_eq!(html.matches("<polygon").count(), 1);
    let first_artifacts = [&out.html, &out.equity_svg, &out.fills_svg, &out.monthly_svg]
        .map(|path| std::fs::read(path).unwrap());
    let second = write_report_html(&summary_path, &sample_summary(), None).unwrap();
    for (path, expected) in [
        &second.html,
        &second.equity_svg,
        &second.fills_svg,
        &second.monthly_svg,
    ]
    .into_iter()
    .zip(first_artifacts)
    {
        assert_eq!(
            std::fs::read(path).unwrap(),
            expected,
            "{} 必须确定性",
            path.display()
        );
    }

    // 曲线文件坏掉必须当场报错，而不是静默产出一份与摘要对不上的图。
    std::fs::write(
        dir.join("run.equity.csv"),
        "index,ts,equity_raw,position_raw\n0,1000,not-a-number,0\n",
    )
    .unwrap();
    let error = write_report_html(&summary_path, &sample_summary(), None).unwrap_err();
    assert!(
        error.contains("equity_raw"),
        "错误要点名坏掉的那一格: {error}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn report_escapes_untrusted_summary_text_and_accepts_custom_html_path() {
    let mut summary = sample_summary();
    summary["strategy_id"] = serde_json::json!("<script>alert(1)</script>");
    summary["input"]["path"] = serde_json::json!("bars <img src=\"http://invalid\"> & more");
    let html = render(&summary, &sample_equity(), &sample_fills());
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(html.contains("&lt;img src=&quot;http://invalid&quot;&gt; &amp; more"));
    assert!(!html.contains("<script"));
    assert!(!html.contains("<img"));

    let dir = temp_dir("custom-output");
    let summary_path = dir.join("run.summary.json");
    std::fs::write(&summary_path, "{}").unwrap();
    let custom = dir.join("reports").join("chosen.HTML");
    let out = write_report_html(&summary_path, &sample_summary(), Some(&custom)).unwrap();
    assert_eq!(out.html, custom);
    assert!(out.html.is_file(), "自定义输出的父目录应自动创建");
    assert_eq!(out.equity_svg.file_name().unwrap(), "chosen.equity.svg");
    assert!(out.equity_svg.is_file() && out.fills_svg.is_file() && out.monthly_svg.is_file());
    assert!(
        write_report_html(
            &summary_path,
            &sample_summary(),
            Some(&dir.join("bad.json"))
        )
        .is_err(),
        "自定义输出必须保留 HTML 扩展名"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn credibility_panel_surfaces_trust_signals_and_flags_uncomputed_fields() {
    // 形状完整的 v4 摘要：六格都应判「可信」，且不该出现「待核」。
    let complete = render(&sample_summary(), &sample_equity(), &sample_fills());
    assert!(
        complete.contains("结果可信度"),
        "报告首页必须含结果可信度面板"
    );
    assert!(complete.contains("输入身份") && complete.contains("重放校验"));
    assert!(complete.contains("撮合档位") && complete.contains("成本模型"));
    assert!(complete.contains("风控规则") && complete.contains("数据质量"));
    assert!(complete.contains("未计算字段"));
    assert!(complete.contains("可信"), "身份与重放等可核对项应判可信");
    assert!(
        complete.contains(">待核<") && complete.contains("未嵌入连续性/缺失值质量报告"),
        "只有数据集身份而没有质量报告时必须明示待核：{}",
        complete
    );
    assert!(
        complete.contains("strategy-bars:BTCUSDT.BINANCE"),
        "数据质量格应印出登记的数据集身份"
    );

    // 缺 metrics 块的旧摘要：未计算字段必须标「待核」并点名缺席指标，而不是假装算过。
    let mut missing = sample_summary();
    missing.as_object_mut().unwrap().remove("metrics");
    let degraded = render(&missing, &sample_equity(), &sample_fills());
    assert!(degraded.contains(">待核<"), "缺指标时必须出现待核状态");
    for name in [
        "return_bps",
        "max_drawdown_bps",
        "fees_raw",
        "turnover_raw",
        "final_equity_raw",
        "sharpe",
        "sortino",
        "calmar",
        "win_rate",
        "profit_factor",
    ] {
        assert!(degraded.contains(name), "未计算字段必须逐一点名：{name}");
    }
}

#[test]
fn year_month_maps_epoch_ms_to_calendar_month() {
    assert_eq!(year_month(0), (1970, 1));
    // 1_700_000_000_000 ms = 2023-11-14 UTC。
    assert_eq!(year_month(1_700_000_000_000), (2023, 11));
    assert_eq!(year_month(1_702_000_000_000), (2023, 12));
}

#[test]
fn monthly_heatmap_labels_every_declared_month() {
    let html = render(&sample_summary(), &sample_equity(), &sample_fills());
    // 三个样本跨 2023-11 与 2023-12 两个月：热力表应出现这两格。
    assert!(html.contains("2023"), "热力表应印出年份");
    assert!(
        html.contains("11") && html.contains("12"),
        "热力表应覆盖两个月"
    );
}
