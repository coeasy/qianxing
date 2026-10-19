use qx_xingban::OrderBookExecutionDescriptor;
use qx_xingban::OrderBookPortfolioBacktestReport;
use std::collections::BTreeMap;

pub(super) fn from_report(
    report: &OrderBookPortfolioBacktestReport,
) -> BTreeMap<String, OrderBookExecutionDescriptor> {
    report
        .execution_models
        .iter()
        .map(|(instrument, descriptor)| (instrument.to_string(), descriptor.clone()))
        .collect()
}
