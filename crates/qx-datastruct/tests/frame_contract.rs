//! BarFrame JSON 文档的版本声明：写侧印的版本必须就是读侧认的那一个（V11 R16）。
//!
//! 此前 `BarFrame::to_json` 是仓里唯一不印 `schema_version` 的帧写侧，而两侧读侧
//! （`qx-data` 的 `parse_bar_frame`、Python 的 `BarFrame.from_json`）都按"缺省即旧格式"
//! 走宽松分支：Rust 自己写出的帧永远拿不到严格口径（`source` 必填、未知字段拒绝），
//! 而它的数据集 Manifest 却已经按当前版本记血缘。这里钉住三件事：写侧声明版本、
//! 读侧对更高版本 fail closed、没有版本的老文档仍然读得回来。

use qx_core::InstrumentId;
use qx_datastruct::{BarFrame, BAR_FRAME_JSON_SCHEMA_VERSION};
use qx_guanxing::{Bar, DataSourceId, DataView};

fn frame() -> BarFrame {
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").expect("valid instrument");
    let view = DataView::try_new(
        vec![
            Bar::new(1, 10, 11, 9, 10, 1),
            Bar::new(2, 11, 12, 10, 11, 2),
        ],
        DataSourceId::new("bars-v1"),
    )
    .expect("view");
    BarFrame::from_view(instrument, &view, 2).expect("frame")
}

#[test]
fn a_written_frame_declares_the_version_its_readers_honour() {
    let written = frame().to_json();
    assert!(
        written.starts_with(&format!(
            "{{\"schema_version\":{BAR_FRAME_JSON_SCHEMA_VERSION},\"instrument\""
        )),
        "帧文档必须把版本号印在第一格，否则两侧读侧都按旧格式走宽松分支：{written}"
    );
    assert_eq!(
        BarFrame::from_json(&written).expect("自己写出的帧必须读得回来"),
        frame()
    );
}

#[test]
fn a_frame_from_a_newer_contract_version_is_refused() {
    let written = frame().to_json();
    let from_the_future = written.replace(
        &format!("\"schema_version\":{BAR_FRAME_JSON_SCHEMA_VERSION}"),
        "\"schema_version\":99",
    );
    let error = BarFrame::from_json(&from_the_future)
        .expect_err("高于本构建支持的版本必须报错，不能降级成宽松解析");
    assert!(
        format!("{error:?}").contains("unsupported BarFrame schema_version"),
        "拒绝必须点名是版本不兼容，实际 {error:?}"
    );
}

#[test]
fn a_versionless_document_still_reads_as_legacy() {
    // 兼容分支不是摆设：R16 之前写出的帧（以及任何没印版本的文档）必须继续读得回来。
    let written = frame().to_json();
    let legacy = written.replace(
        &format!("\"schema_version\":{BAR_FRAME_JSON_SCHEMA_VERSION},"),
        "",
    );
    assert!(!legacy.contains("schema_version"), "夹具没剥干净: {legacy}");
    assert_eq!(
        BarFrame::from_json(&legacy).expect("旧文档仍要走兼容分支"),
        frame()
    );
}
