//! BarFrame JSON 文档的版本声明：写侧印的版本必须就是读侧认的那一个（V11 R16）。
//!
//! 此前 `BarFrame::to_json` 是仓里唯一不印 `schema_version` 的帧写侧，而两侧读侧
//! （`qx-data` 的 `parse_bar_frame`、Python 的 `BarFrame.from_json`）都按"缺省即旧格式"
//! 走宽松分支：Rust 自己写出的帧永远拿不到严格口径（`source` 必填、未知字段拒绝），
//! 而它的数据集 Manifest 却已经按当前版本记血缘。这里钉住三件事：写侧声明版本、
//! 读侧对更高版本 fail closed、没有版本的老文档仍然读得回来。

use qx_core::{ExactDecimal, ExactOhlcv, InstrumentId};
use qx_datastruct::BarFrame;
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

fn decimal(value: &str) -> ExactDecimal {
    ExactDecimal::parse(value).expect("valid exact decimal")
}

fn exact_frame() -> BarFrame {
    let exact = ExactOhlcv {
        open: decimal("1.0000000001"),
        high: decimal("1.0000000003"),
        low: decimal("1.0000000001"),
        close: decimal("1.0000000002"),
        volume: decimal("1.00000000005"),
    };
    let bar = Bar::new(
        1,
        1_000_000_000,
        1_000_000_000,
        1_000_000_000,
        1_000_000_000,
        1_000_000_000,
    )
    .with_exact_ohlcv(exact)
    .expect("sub-nanounit OHLCV projects to compatible legacy raw values");
    let view =
        DataView::try_new(vec![bar], DataSourceId::new("bars-v2")).expect("view with exact row");
    BarFrame::from_view(
        InstrumentId::parse("TINYUSDT.BINANCE").expect("valid instrument"),
        &view,
        1,
    )
    .expect("exact frame")
}

#[test]
fn a_written_frame_declares_the_version_its_readers_honour() {
    let written = frame().to_json();
    let schema_version = 1;
    assert!(
        written.starts_with(&format!(
            "{{\"schema_version\":{schema_version},\"instrument\""
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
    let from_the_future = written.replace("\"schema_version\":1", "\"schema_version\":99");
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
    let legacy = written.replace("\"schema_version\":1,", "");
    assert!(!legacy.contains("schema_version"), "夹具没剥干净: {legacy}");
    assert_eq!(
        BarFrame::from_json(&legacy).expect("旧文档仍要走兼容分支"),
        frame()
    );
}

#[test]
fn legacy_unknown_fields_remain_compatible_but_versioned_fields_are_strict() {
    let mut legacy: serde_json::Value = serde_json::from_str(&frame().to_json()).unwrap();
    legacy.as_object_mut().unwrap().remove("schema_version");
    legacy["legacy_extension"] = serde_json::json!({"ignored": [1, 2, 3]});
    assert_eq!(
        BarFrame::from_json(&legacy.to_string()).expect("legacy extension remains compatible"),
        frame()
    );

    let mut versioned = legacy;
    versioned["schema_version"] = serde_json::json!(1);
    let error = BarFrame::from_json(&versioned.to_string())
        .expect_err("versioned documents reject unknown fields");
    assert!(format!("{error:?}").contains("contains unknown field legacy_extension"));
}

#[test]
fn v2_round_trip_preserves_sub_nano_ohlcv_and_resampling_precision() {
    let frame = exact_frame();
    let written = frame.to_json();
    assert!(written.contains("\"schema_version\":2"));
    let restored = BarFrame::from_json(&written).expect("v2 exact frame reads back");
    assert_eq!(restored, frame);
    assert_eq!(restored.digest(), frame.digest());
    assert_eq!(
        Vec::<Bar>::from(&restored)[0].close_exact(),
        decimal("1.0000000002")
    );

    let second = ExactOhlcv {
        open: decimal("1.0000000002"),
        high: decimal("1.0000000004"),
        low: decimal("1.0000000002"),
        close: decimal("1.0000000003"),
        volume: decimal("1.00000000007"),
    };
    let mut bars = Vec::<Bar>::from(&frame);
    bars.push(
        Bar::new(
            2,
            1_000_000_000,
            1_000_000_000,
            1_000_000_000,
            1_000_000_000,
            1_000_000_000,
        )
        .with_exact_ohlcv(second)
        .expect("second exact bar"),
    );
    let view = DataView::try_new(bars, DataSourceId::new("bars-v2")).expect("monotonic exact bars");
    let two_rows = BarFrame::from_view(frame.instrument.clone(), &view, 2).unwrap();
    let resampled = two_rows.resample(3).expect("exact resample");
    assert_eq!(
        resampled.exact_ohlcv.as_ref().unwrap()[0].volume,
        decimal("2.00000000012")
    );
    assert_eq!(resampled.volume_raw, vec![2_000_000_000]);
}

#[test]
fn v2_rejects_mismatched_legacy_projection() {
    let mut value: serde_json::Value = serde_json::from_str(&exact_frame().to_json()).unwrap();
    value["close_raw"][0] = serde_json::json!(1);
    let error = BarFrame::from_json(&value.to_string()).expect_err("projection mismatch rejected");
    assert!(format!("{error:?}").contains("raw projection mismatch"));
}
