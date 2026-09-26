use crate::schema::Bar;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValidationReport {
    pub checked: usize,
    pub errors: Vec<String>,
}

impl ValidationReport {
    pub fn ok(checked: usize) -> Self {
        Self {
            checked,
            errors: Vec::new(),
        }
    }

    pub fn valid(&self) -> bool {
        self.errors.is_empty()
    }
}

pub fn validate_bars(bars: &[Bar]) -> ValidationReport {
    let mut report = ValidationReport::ok(bars.len());
    let mut last_timestamp = BTreeMap::<&str, u64>::new();

    for (index, bar) in bars.iter().enumerate() {
        if let Err(error) = bar.validate() {
            report.errors.push(format!("row {index}: {error}"));
            continue;
        }
        if bar.volume_raw < 0 {
            report
                .errors
                .push(format!("row {index}: volume must be non-negative"));
        }
        if bar.high_raw < bar.low_raw
            || bar.high_raw < bar.open_raw
            || bar.high_raw < bar.close_raw
            || bar.low_raw > bar.open_raw
            || bar.low_raw > bar.close_raw
        {
            report
                .errors
                .push(format!("row {index}: invalid OHLC range"));
        }
        if let Some(previous) = last_timestamp.insert(bar.instrument.as_str(), bar.timestamp) {
            if previous >= bar.timestamp {
                report.errors.push(format!(
                    "row {index}: timestamps for {} must be strictly increasing",
                    bar.instrument
                ));
            }
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts: u64) -> Bar {
        Bar {
            instrument: "XSHG:600000".into(),
            timestamp: ts,
            open_raw: 10,
            high_raw: 12,
            low_raw: 9,
            close_raw: 11,
            volume_raw: 100,
        }
    }

    #[test]
    fn validates_per_instrument_time_order() {
        let report = validate_bars(&[bar(2), bar(1)]);
        assert!(!report.valid());
    }

    #[test]
    fn rejects_invalid_ohlc() {
        let mut invalid = bar(1);
        invalid.high_raw = 8;
        let report = validate_bars(&[invalid]);
        assert!(!report.valid());
    }
}
