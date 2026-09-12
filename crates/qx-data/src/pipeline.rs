use crate::schema::Bar;
use crate::validation::ValidationReport;

pub fn validate_bars(bars: &[Bar]) -> ValidationReport {
    let mut report = ValidationReport::default();

    for window in bars.windows(2) {
        if window[0].timestamp >= window[1].timestamp {
            report.errors.push("timestamps must be increasing".to_string());
        }
    }

    report
}
