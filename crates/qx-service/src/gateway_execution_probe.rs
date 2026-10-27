//! Business health observations at the shared Venue submit/cancel boundary.

use crate::exchange_gateway::{ExchangeGatewayPort, GatewayBusinessHealthStatus};
use crate::pipeline_metrics_report::PipelineMetricsReporter;
use qx_core::{ErrorCode, Order, QxError, QxResult, TradingInstrumentSpec};
use qx_zhenlu::{Venue, VenueEvent, VenueOrderSnapshot};

pub(crate) struct GatewayExecutionProbeVenue<'a, V> {
    venue: V,
    metrics: &'a mut PipelineMetricsReporter,
}

impl<'a, V> GatewayExecutionProbeVenue<'a, V> {
    pub(crate) fn new(venue: V, metrics: &'a mut PipelineMetricsReporter) -> Self {
        Self { venue, metrics }
    }

    fn observe(&mut self, status: GatewayBusinessHealthStatus, observed_ts_ms: u64) {
        self.metrics.observe_gateway_port_business_status(
            ExchangeGatewayPort::VenueExecution,
            status,
            observed_ts_ms,
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
        let result = self.venue.submit(order, ts);
        self.observe(execution_result_status(&result), ts);
        result
    }

    fn cancel(&mut self, client_order_id: u64, ts: u64) -> QxResult<Vec<VenueEvent>> {
        let result = self.venue.cancel(client_order_id, ts);
        self.observe(execution_result_status(&result), ts);
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
