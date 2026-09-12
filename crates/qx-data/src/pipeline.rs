use crate::schema::Bar;
use crate::validation::{validate_bars, ValidationReport};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPipelineReport {
    pub rows: usize,
    pub validation: ValidationReport,
}

pub fn process_bars(mut bars: Vec<Bar>) -> Result<(Vec<Bar>, DataPipelineReport), String> {
    bars.sort_by(|left, right| {
        left.instrument
            .cmp(&right.instrument)
            .then(left.timestamp.cmp(&right.timestamp))
    });

    let validation = validate_bars(&bars);
    if !validation.valid() {
        return Err(validation.errors.join("; "));
    }

    let rows = bars.len();
    Ok((bars, DataPipelineReport { rows, validation }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(instrument: &str, timestamp: u64) -> Bar {
        Bar {
            instrument: instrument.into(),
            timestamp,
            open_raw: 10,
            high_raw: 12,
            low_raw: 9,
            close_raw: 11,
            volume_raw: 100,
        }
    }

    #[test]
    fn canonicalizes_before_validation() {
        let input = vec![bar("XSHG:600000", 2), bar("XSHG:600000", 1)];
        let (output, report) = process_bars(input).unwrap();
        assert!(report.validation.valid());
        assert_eq!(output[0].timestamp, 1);
        assert_eq!(output[1].timestamp, 2);
    }

    #[test]
    fn canonicalizes_across_instruments() {
        let input = vec![bar("XSHG:600000", 1), bar("XNAS:MSFT", 1)];
        let (output, _) = process_bars(input).unwrap();
        assert_eq!(output[0].instrument, "XNAS:MSFT");
    }
}
