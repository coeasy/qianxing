//! Cross-platform stable fingerprints for canonical datasets.

use crate::pipeline::process_bars;
use crate::schema::Bar;
use qx_datastruct::BarFrame;

struct StableFnv1a {
    value: u64,
}

impl StableFnv1a {
    fn new() -> Self {
        Self {
            value: 0xcbf2_9ce4_8422_2325,
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.value ^= *byte as u64;
            self.value = self.value.wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn write_text(&mut self, value: &str) {
        self.write_bytes(&(value.len() as u64).to_le_bytes());
        self.write_bytes(value.as_bytes());
    }

    fn finish(self) -> u64 {
        self.value
    }
}

pub fn fingerprint_bars(bars: &[Bar]) -> Result<String, String> {
    let (canonical, _) = process_bars(bars.to_vec())?;
    let mut hash = StableFnv1a::new();
    hash.write_bytes(&(canonical.len() as u64).to_le_bytes());
    for bar in canonical {
        hash.write_text(&bar.instrument);
        hash.write_bytes(&bar.timestamp.to_le_bytes());
        hash.write_bytes(&bar.open_raw.to_le_bytes());
        hash.write_bytes(&bar.high_raw.to_le_bytes());
        hash.write_bytes(&bar.low_raw.to_le_bytes());
        hash.write_bytes(&bar.close_raw.to_le_bytes());
        hash.write_bytes(&bar.volume_raw.to_le_bytes());
        if let Some(exact) = bar.exact_ohlcv {
            hash.write_bytes(&1_u64.to_le_bytes());
            for value in [exact.open, exact.high, exact.low, exact.close, exact.volume] {
                hash.write_text(&value.to_string());
            }
        }
    }
    Ok(format!("{:016x}", hash.finish()))
}

/// Apply the canonical dataset fingerprint to an already validated BarFrame.
/// This is an adapter only; `fingerprint_bars` remains the sole hashing algorithm.
pub fn fingerprint_bar_frame(frame: &BarFrame) -> Result<String, String> {
    let instrument = frame.instrument.to_string();
    let bars = (0..frame.len())
        .map(|index| Bar {
            instrument: instrument.clone(),
            timestamp: frame.ts[index],
            open_raw: frame.open_raw[index],
            high_raw: frame.high_raw[index],
            low_raw: frame.low_raw[index],
            close_raw: frame.close_raw[index],
            volume_raw: frame.volume_raw[index],
            exact_ohlcv: frame.exact_ohlcv.as_ref().map(|values| values[index]),
        })
        .collect::<Vec<_>>();
    fingerprint_bars(&bars)
}

/// Give each normalized BarFrame byte snapshot an immutable registry version while retaining
/// the stable provider/schema version as a readable prefix. The v1 constant alone used to be
/// the registry version too, which made legitimate refreshed provider data collide with an
/// earlier manifest for the same instrument.
pub fn barframe_snapshot_version(frame: &BarFrame) -> String {
    let content_hash = crate::sha256_hex_bytes(frame.to_json().as_bytes());
    format!("{}+sha256:{content_hash}", crate::BARFRAME_DATASET_VERSION)
}

/// Registry version for a BarFrame source. Local immutable fixtures keep the legacy provider
/// version; CCXT snapshots are re-fetched and can legitimately revise, so each distinct source
/// payload gets its own immutable version.
pub fn barframe_registry_version(frame: &BarFrame) -> String {
    if frame.source.0.starts_with("ccxt:") {
        barframe_snapshot_version(frame)
    } else {
        crate::BARFRAME_DATASET_VERSION.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{ExactDecimal, ExactOhlcv};

    fn bar(ts: u64, close_raw: i128) -> Bar {
        Bar {
            instrument: "XSHG:600000".into(),
            timestamp: ts,
            open_raw: close_raw,
            high_raw: close_raw,
            low_raw: close_raw,
            close_raw,
            volume_raw: 100,
            exact_ohlcv: None,
        }
    }

    #[test]
    fn fingerprint_is_order_independent_after_canonicalization() {
        let forward = vec![bar(1, 10), bar(2, 11)];
        let reverse = vec![bar(2, 11), bar(1, 10)];
        assert_eq!(
            fingerprint_bars(&forward).unwrap(),
            fingerprint_bars(&reverse).unwrap()
        );
    }

    #[test]
    fn fingerprint_changes_when_data_changes() {
        assert_ne!(
            fingerprint_bars(&[bar(1, 10)]).unwrap(),
            fingerprint_bars(&[bar(1, 11)]).unwrap()
        );
    }

    #[test]
    fn fingerprint_changes_when_sub_nano_source_data_changes() {
        let mut coarse = Bar {
            instrument: "XSHG:600000".into(),
            timestamp: 1,
            open_raw: 10,
            high_raw: 12,
            low_raw: 9,
            close_raw: 10,
            volume_raw: 100,
            exact_ohlcv: None,
        };
        let precise = |close: &str| ExactOhlcv {
            open: ExactDecimal::parse("0.0000000101").unwrap(),
            high: ExactDecimal::parse("0.0000000129").unwrap(),
            low: ExactDecimal::parse("0.0000000095").unwrap(),
            close: ExactDecimal::parse(close).unwrap(),
            volume: ExactDecimal::parse("0.0000001008").unwrap(),
        };
        coarse.exact_ohlcv = Some(precise("0.0000000101"));
        let mut revised = coarse.clone();
        revised.exact_ohlcv = Some(precise("0.0000000102"));
        assert_eq!(coarse.close_raw, revised.close_raw);
        assert_ne!(
            fingerprint_bars(&[coarse]).unwrap(),
            fingerprint_bars(&[revised]).unwrap()
        );
    }

    #[test]
    fn frame_adapter_delegates_to_the_canonical_bar_fingerprint() {
        let frame = BarFrame::from_json(
            r#"{"schema_version":1,"instrument":"BTCUSDT.BINANCE","source":"fixture@v1","ts":[60000,120000],"open_raw":[10,11],"high_raw":[12,13],"low_raw":[9,10],"close_raw":[11,12],"volume_raw":[1,2]}"#,
        )
        .unwrap();
        let expected =
            fingerprint_bars(&[bar_for_frame(&frame, 0), bar_for_frame(&frame, 1)]).unwrap();
        assert_eq!(fingerprint_bar_frame(&frame).unwrap(), expected);
    }

    #[test]
    fn snapshot_version_is_stable_for_same_bytes_and_changes_with_content() {
        let original = BarFrame::from_json(
            r#"{"schema_version":1,"instrument":"BTCUSDT.BINANCE","source":"ccxt:binance","ts":[60000,120000],"open_raw":[10,11],"high_raw":[12,13],"low_raw":[9,10],"close_raw":[11,12],"volume_raw":[1,2]}"#,
        )
        .unwrap();
        let identical = original.clone();
        let mut revised = original.clone();
        revised.close_raw[1] += 1;

        let version = barframe_snapshot_version(&original);
        assert_eq!(version, barframe_snapshot_version(&identical));
        assert_ne!(version, barframe_snapshot_version(&revised));
        assert!(version.starts_with("barframe-json-v1+sha256:"));
    }

    fn bar_for_frame(frame: &BarFrame, index: usize) -> Bar {
        Bar {
            instrument: frame.instrument.to_string(),
            timestamp: frame.ts[index],
            open_raw: frame.open_raw[index],
            high_raw: frame.high_raw[index],
            low_raw: frame.low_raw[index],
            close_raw: frame.close_raw[index],
            volume_raw: frame.volume_raw[index],
            exact_ohlcv: frame.exact_ohlcv.as_ref().map(|values| values[index]),
        }
    }
}
