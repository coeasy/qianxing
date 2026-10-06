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
            "final_equity_raw": 100125000000000_i64
        },
        "matching_kernel": "qx-xingban::BacktestEngine(bar)",
        "risk_rules": { "rule_set_version": "v1", "source": "runtime-config" },
        "execution_costs": { "source": "runtime-config-default" },
        "replay": { "log_digest": "feedface", "events": 7, "ledger_entries": 4 },
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
fn report_is_byte_equal_across_runs() {
    let summary = sample_summary();
    let first = render_report_html(&summary, &sample_equity(), &sample_fills());
    let second = render_report_html(&summary, &sample_equity(), &sample_fills());
    assert_eq!(first, second, "同输入两次渲染必须逐字节相等");
    assert!(first.starts_with("<!DOCTYPE html>"));
    assert!(first.contains("牵星回测报告"));
}

#[test]
fn report_has_no_external_resource_reference() {
    let html = render_report_html(&sample_summary(), &sample_equity(), &sample_fills());
    // 内联 SVG 不带 `xmlns`、样式全内联：整篇不出现 http/https，就没有意外外链。
    assert!(!html.contains("http"), "报告不得出现任何 http 引用");
    assert!(!html.contains("src="), "报告不得引入外部脚本/图片");
    assert!(html.contains("<svg"), "三张图必须内嵌");
}

#[test]
fn zero_fills_draw_no_marker_and_each_fill_adds_one() {
    let summary = sample_summary();
    let none = render_report_html(&summary, &sample_equity(), &[]);
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
    let both = render_report_html(&summary, &sample_equity(), &two);
    assert_eq!(both.matches("<polygon").count(), 2, "每笔成交一个标记");
    // 买红卖绿（中国市场口径）。
    assert!(both.contains("#c0392b") && both.contains("#1e8449"));
}

#[test]
fn cards_read_from_summary_fields_only() {
    let html = render_report_html(&sample_summary(), &sample_equity(), &sample_fills());
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
    let out = write_report_html(&summary_path, &sample_summary()).unwrap();
    assert_eq!(
        out.file_name().unwrap().to_str().unwrap(),
        "run.report.html"
    );
    let html = std::fs::read_to_string(&out).unwrap();
    assert_eq!(html.matches("<polygon").count(), 1);

    // 曲线文件坏掉必须当场报错，而不是静默产出一份与摘要对不上的图。
    std::fs::write(
        dir.join("run.equity.csv"),
        "index,ts,equity_raw,position_raw\n0,1000,not-a-number,0\n",
    )
    .unwrap();
    let error = write_report_html(&summary_path, &sample_summary()).unwrap_err();
    assert!(
        error.contains("equity_raw"),
        "错误要点名坏掉的那一格: {error}"
    );
    let _ = std::fs::remove_dir_all(&dir);
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
    let html = render_report_html(&sample_summary(), &sample_equity(), &sample_fills());
    // 三个样本跨 2023-11 与 2023-12 两个月：热力表应出现这两格。
    assert!(html.contains("2023"), "热力表应印出年份");
    assert!(
        html.contains("11") && html.contains("12"),
        "热力表应覆盖两个月"
    );
}
