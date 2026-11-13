//! Business health observations at the shared Venue submit/cancel boundary.

use crate::exchange_gateway::{ExchangeGatewayPort, GatewayBusinessHealthStatus};
use crate::pipeline_metrics_report::PipelineMetricsReporter;
use qx_core::{ErrorCode, Order, QxError, QxResult, TradingInstrumentSpec};
use qx_zhenlu::{Venue, VenueEvent, VenueOrderSnapshot};
use std::time::Instant;

pub(crate) struct GatewayExecutionProbeVenue<'a, V> {
    venue: V,
    metrics: &'a mut PipelineMetricsReporter,
}

impl<'a, V> GatewayExecutionProbeVenue<'a, V> {
    pub(crate) fn new(venue: V, metrics: &'a mut PipelineMetricsReporter) -> Self {
        Self { venue, metrics }
    }

    pub(crate) fn into_inner(self) -> V {
        self.venue
    }

    fn observe(
        &mut self,
        status: GatewayBusinessHealthStatus,
        observed_ts_ms: u64,
        duration: std::time::Duration,
        error_category: Option<&'static str>,
    ) {
        self.metrics.observe_gateway_port_business_status(
            ExchangeGatewayPort::VenueExecution,
            status,
            observed_ts_ms,
        );
        if let Some(category) = error_category {
            self.metrics
                .observe_gateway_port_error_category(ExchangeGatewayPort::VenueExecution, category);
        }
        self.metrics.observe_gateway_port_operation_duration(
            ExchangeGatewayPort::VenueExecution,
            duration.as_nanos().min(u128::from(u64::MAX)) as u64,
        );
    }
}

impl<V: Venue> Venue for GatewayExecutionProbeVenue<'_, V> {
    fn id(&self) -> &str {
        self.venue.id()
    }

    fn capabilities(&self) -> &'static qx_zhenlu::venue_capabilities::VenueCapabilities {
        self.venue.capabilities()
    }

    fn validate_instrument_spec(&mut self, spec: &TradingInstrumentSpec) -> QxResult<()> {
        self.venue.validate_instrument_spec(spec)
    }

    fn validate_order_constraints(
        &mut self,
        spec: &TradingInstrumentSpec,
        order: &Order,
    ) -> QxResult<()> {
        self.venue.validate_order_constraints(spec, order)
    }

    fn execution_readiness(&self) -> QxResult<()> {
        self.venue.execution_readiness()
    }

    fn submit(&mut self, order: Order, ts: u64) -> QxResult<Vec<VenueEvent>> {
        let started = Instant::now();
        let result = self.venue.submit(order, ts);
        self.observe(
            execution_result_status(&result),
            ts,
            started.elapsed(),
            result.as_ref().err().map(execution_error_category),
        );
        result
    }

    fn cancel(&mut self, client_order_id: u64, ts: u64) -> QxResult<Vec<VenueEvent>> {
        let started = Instant::now();
        let result = self.venue.cancel(client_order_id, ts);
        self.observe(
            execution_result_status(&result),
            ts,
            started.elapsed(),
            result.as_ref().err().map(execution_error_category),
        );
        result
    }

    fn validate_amendment(&mut self, current: &Order, amended: &Order) -> QxResult<()> {
        self.venue.validate_amendment(current, amended)
    }

    fn amend(&mut self, current: &Order, amended: &Order, ts: u64) -> QxResult<Vec<VenueEvent>> {
        let started = Instant::now();
        let result = self.venue.amend(current, amended, ts);
        self.observe(
            execution_result_status(&result),
            ts,
            started.elapsed(),
            result.as_ref().err().map(execution_error_category),
        );
        result
    }

    fn snapshot(&self) -> Vec<VenueOrderSnapshot> {
        self.venue.snapshot()
    }

    fn connected(&self) -> bool {
        self.venue.connected()
    }
}

fn execution_result_status<T>(result: &QxResult<T>) -> GatewayBusinessHealthStatus {
    match result {
        Ok(_) => GatewayBusinessHealthStatus::Healthy,
        Err(error) if execution_result_is_ambiguous(error) => {
            GatewayBusinessHealthStatus::Unhealthy
        }
        Err(_) => GatewayBusinessHealthStatus::Degraded,
    }
}

fn execution_error_category(error: &QxError) -> &'static str {
    match error.contract().code {
        ErrorCode::Transient => "retryable",
        ErrorCode::Permanent => "permanent",
        ErrorCode::Ambiguous | ErrorCode::ReconcileRequired => "ambiguous",
        ErrorCode::VenueState => "exchange",
        ErrorCode::ResourceExhausted => "rate_limited",
        ErrorCode::BusinessViolation => "business_violation",
        ErrorCode::Invariant => "invariant",
    }
}

fn execution_result_is_ambiguous(error: &QxError) -> bool {
    matches!(
        error.contract().code,
        ErrorCode::Ambiguous | ErrorCode::ReconcileRequired | ErrorCode::Invariant
    )
}

#[cfg(test)]
mod tests {
    use super::{execution_result_is_ambiguous, execution_result_status};
    use crate::exchange_gateway::GatewayBusinessHealthStatus;
    use qx_core::QxError;

    #[test]
    fn venue_execution_probe_distinguishes_success_rejection_and_unknown_outcome() {
        assert_eq!(
            execution_result_status::<()>(&Ok(())),
            GatewayBusinessHealthStatus::Healthy
        );
        assert_eq!(
            execution_result_status::<()>(&Err(QxError::BusinessViolation(
                "交易所拒绝订单".into()
            ))),
            GatewayBusinessHealthStatus::Degraded
        );
        assert_eq!(
            execution_result_status::<()>(&Err(QxError::Ambiguous("请求超时".into()))),
            GatewayBusinessHealthStatus::Unhealthy
        );
        assert_eq!(
            execution_result_status::<()>(&Err(QxError::Invariant("状态无法确认".into()))),
            GatewayBusinessHealthStatus::Unhealthy
        );
        assert!(execution_result_is_ambiguous(&QxError::ReconcileRequired(
            "需要重新对账".into()
        )));
    }
}
