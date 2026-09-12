//! Deterministic incremental merge for canonical bars.

use crate::schema::Bar;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IncrementalMergeReport {
    pub existing_rows: usize,
    pub incoming_rows: usize,
    pub inserted_rows: usize,
    pub replaced_rows: usize,
    pub output_rows: usize,
}

pub fn merge_bars(
    existing: &[Bar],
    incoming: &[Bar],
) -> Result<(Vec<Bar>, IncrementalMergeReport), String> {
    let mut merged: BTreeMap<(String, u64), Bar> = BTreeMap::new();

    for bar in existing {
        bar.validate()?;
        merged.insert((bar.instrument.clone(), bar.timestamp), bar.clone());
    }

    let mut report = IncrementalMergeReport {
        existing_rows: existing.len(),
        incoming_rows: incoming.len(),
        ..IncrementalMergeReport::default()
    };

    for bar in incoming {
        bar.validate()?;
        let key = (bar.instrument.clone(), bar.timestamp);
        if merged.insert(key, bar.clone()).is_some() {
            report.replaced_rows += 1;
        } else {
            report.inserted_rows += 1;
        }
    }

    let output: Vec<Bar> = merged.into_values().collect();
    report.output_rows = output.len();
    Ok((output, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(instrument: &str, ts: u64, close_raw: i128) -> Bar {
        Bar {
            instrument: instrument.into(),
            timestamp: ts,
            open_raw: close_raw,
            high_raw: close_raw,
            low_raw: close_raw,
            close_raw,
            volume_raw: 1,
        }
    }

    #[test]
    fn incoming_rows_replace_same_identity_and_append_new_rows() {
        let existing = vec![bar("XSHG:600000", 1, 10), bar("XSHG:600000", 2, 20)];
        let incoming = vec![bar("XSHG:600000", 2, 21), bar("XSHG:600000", 3, 30)];

        let (merged, report) = merge_bars(&existing, &incoming).unwrap();

        assert_eq!(report.existing_rows, 2);
        assert_eq!(report.incoming_rows, 2);
        assert_eq!(report.replaced_rows, 1);
        assert_eq!(report.inserted_rows, 1);
        assert_eq!(report.output_rows, 3);
        assert_eq!(merged[1].close_raw, 21);
        assert_eq!(merged[2].timestamp, 3);
    }

    #[test]
    fn merge_order_is_stable_across_instruments() {
        let incoming = vec![bar("XNAS:MSFT", 2, 2), bar("XSHG:600000", 1, 1)];
        let (merged, _) = merge_bars(&[], &incoming).unwrap();

        assert_eq!(merged[0].instrument, "XNAS:MSFT");
        assert_eq!(merged[1].instrument, "XSHG:600000");
    }
}
