//! Cross-platform stable fingerprints for canonical datasets.

use crate::pipeline::process_bars;
use crate::schema::Bar;

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
    }
    Ok(format!("{:016x}", hash.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts: u64, close_raw: i128) -> Bar {
        Bar {
            instrument: "XSHG:600000".into(),
            timestamp: ts,
            open_raw: close_raw,
            high_raw: close_raw,
            low_raw: close_raw,
            close_raw,
            volume_raw: 100,
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
}
