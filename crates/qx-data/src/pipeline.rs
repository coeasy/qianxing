use crate::schema::Bar;
use crate::validation::ValidationReport;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPipelineReport {
    pub rows: usize,
    pub validation: ValidationReport,
}

pub fn process_bars(mut bars: Vec<Bar>) -> Result<(Vec<Bar>, DataPipelineReport), String> {
    let validation = validate_bars(&bars);
    if !validation.errors.is_empty() {
        return Err(validation.errors.join("; "));
    }

    bars.sort_by_key(|bar| bar.timestamp);
    let rows = bars.len();
    Ok((bars, DataPipelineReport { rows, validation }))
}
