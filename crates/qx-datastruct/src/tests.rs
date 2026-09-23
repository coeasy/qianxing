//! 帧的列式边界与转换 Manifest 的同源用例。
//!
//! V11 R16 起从 `lib.rs` 外置：行数预算只降不升，写侧多印一格契约版本就得在别处节省出来。

use super::*;

#[test]
fn frame_is_pit_bounded_and_round_trips_columns() {
    let instrument = InstrumentId::parse("T.SIM").unwrap();
    let view = DataView::try_new(
        vec![
            Bar::new(1, 10, 11, 9, 10, 1),
            Bar::new(2, 11, 12, 10, 11, 2),
            Bar::new(3, 12, 13, 11, 12, 3),
        ],
        DataSourceId::new("bars-v1"),
    )
    .unwrap();
    let frame = BarFrame::from_view(instrument, &view, 2).unwrap();
    assert_eq!(frame.len(), 2);
    assert_eq!(frame.close_at(1), Some(11));
    assert_eq!(Vec::<Bar>::from(&frame)[1].ts, 2);
    assert!(frame.to_json().contains("\"volume_raw\":[1,2]"));
    assert_eq!(BarFrame::from_json(&frame.to_json()).unwrap(), frame);
    assert_eq!(frame.digest(), 0xf9d5_8b91_e72d_ae58);
    let selected = frame.select_time(2, 2).unwrap();
    assert_eq!(selected.ts, vec![2]);
    let resampled = frame.resample(2).unwrap();
    assert_eq!(resampled.ts, vec![0, 2]);
    assert_eq!(resampled.volume_raw, vec![1, 2]);
    assert_eq!(frame.resample(0), Err(FrameError::InvalidInterval));
    let (selected, manifest) = frame.select_time_with_manifest(2, 3).unwrap();
    assert_eq!(manifest.operation, "select_time");
    assert_eq!(manifest.input_hash, frame.digest());
    assert_eq!(manifest.output_hash, selected.digest());
    assert_eq!(
        TransformManifest::from_json(&manifest.to_json().unwrap()).unwrap(),
        manifest
    );
    let (_, resample_manifest) = frame.resample_with_manifest(2).unwrap();
    assert_eq!(resample_manifest.parameters["interval"], "2");
}

#[test]
fn arrow_views_are_zero_copy_and_reject_decimal_overflow() {
    let instrument = InstrumentId::parse("T.SIM").unwrap();
    let view = DataView::try_new(
        vec![
            Bar::new(1, 10, 11, 9, 10, 1),
            Bar::new(2, 11, 12, 10, 11, 2),
        ],
        DataSourceId::new("bars-v1"),
    )
    .unwrap();
    let frame = BarFrame::from_view(instrument, &view, 2).unwrap();
    let columns = frame.arrow_column_views().unwrap();
    assert_eq!(columns.len(), 6);
    assert_eq!(columns[0].name(), "ts");
    assert_eq!(columns[0].array().length, 2);
    assert_eq!(columns[0].data_ptr(), frame.ts.as_ptr().cast());
    assert_eq!(columns[1].name(), "open_raw");
    assert_eq!(columns[1].data_ptr(), frame.open_raw.as_ptr().cast());
    assert_eq!(
        columns[1].schema().format,
        ARROW_FORMAT_DECIMAL128.as_ptr().cast()
    );

    let mut extreme = frame.clone();
    extreme.open_raw[0] = i128::MAX;
    assert!(matches!(
        extreme.arrow_column_views(),
        Err(FrameError::ArrowDecimalOverflow)
    ));

    let mut owned = frame.owned_arrow_columns().unwrap();
    let owned_open = owned.remove(1);
    let (mut array, mut schema) = owned_open.into_ffi();
    assert!(array.release.is_some());
    assert!(schema.release.is_some());
    unsafe {
        (array.release.expect("array release callback"))(&mut array);
        (schema.release.expect("schema release callback"))(&mut schema);
    }
    assert!(array.release.is_none());
    assert!(schema.release.is_none());
}
