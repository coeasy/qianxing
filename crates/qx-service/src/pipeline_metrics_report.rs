//! 长驻 Paper/venue worker 的 pipeline 计数与 ingest 延迟出口（V13 §9.27 #178 / D8）。
//!
//! `LiveEventPipeline` 的六个计数（`ingest_attempts`/`ingested_events`/`deduplicated_events`/
//! `transient_retries`/`refreshes`/`failures`）和 ingest 累计/最大耗时由 runtime 采集；可第八遍立案时
//! 全仓对 `metrics()` 的调用只有那个 crate 自己的用例 —— 数据在生产里被算出来再丢掉。当时不接线
//! 是因为量纲未定：计数按 pipeline 对象自打开起累计，而 paper 循环里行情按命令、恢复按 tick 各开
//! 一个对象，直接印成 `_total` 就是给抓取端一条每次请求归零的「累计计数」，比不印更糟。
//!
//! 这里把量纲定成**按 worker 进程**：短生命周期 pipeline 在不再写入时并账一次；长驻 pipeline 按快照差值
//! 增量并账。worker 循环把累计量和 100ms 单次 ingest 预算状态写进 `worker-metrics/<worker_id>.prom`，
//! 高频流出口最多每秒写一次。归零边界收在进程重启，与 `qx_worker_up`、心跳同一口径，也正是抓取端
//! 对 counter 重启的标准处理。
//!
//! 不在这个出口里计的站点逐名登记在 `tests/pipeline_metrics_open_sites.rs` 的豁免名单里，并由那条
//! 判据与本段双向核对 —— 名单改了文档不改、或者反过来，都会红：
//! `open_account_pipeline`（统一入口委托）与 `open_runtime_pipeline`（共享底层打开器，不归单个 worker 进程）、
//! `qx-service::api_host::hydrate_api_initial_state_with_market_specs`、`query_api_models`、`load_api_account_snapshot_for_worker_with_market_specs`、`qx-service::api_host::spawn_api_projection_bridge`、`handle_algo_execution_request`、
//! `strategy_current_qty_for_account`、`strategy_current_qty_exact_for_account`、`strategy_account_context`、
//! `collect_pending`（策略 worker 分别读取行情与账户 EventLog 的一次性只读快照）、
//! `execution_updates_for_ranges`（策略重试分别按行情与账户 EventLog 的作业范围读取一次性只读快照）、
//! `live_strategy_input_digest`（调度器读取最新行情 EventLog 生成策略输入触发摘要）、
//! `populate_strategy_execution_positions`（策略服务按请求打开的只读持仓投影 pipeline，
//! 存活期与「按 worker 进程累计」不符，接它要另立一份跨请求的身份），以及
//! `run_paper_pipeline_once_with_report`、`run_paper_submit_order` 与它拆出的 `paper_submit_action`
//! （一次性验收入口，不是长驻 worker：
//! 给它们落 `.prom` 等于替一个已经退出的进程宣称健康。它们写入的事实照旧进 EventLog，
//! 只是不在这族样本里 —— `paper-submit-order` 那条还会以 `paper-execution` 的名义领取租约，
//! 所以它的计数并进 worker 会更假）。Binance/CCXT 的 MarketData、UserStream、Execution、Reconciler、
//! SpreadRecovery 与 Paper 行情镜像 pipeline 由相应 worker 循环通过 `observe`/`absorb` 计入同一出口，
//! 不经过上述短生命周期 account-pipeline 站点表。

use std::collections::BTreeMap;
use std::path::PathBuf;

use qx_runtime::{
    LiveEventPipeline, PipelineMetricsSnapshot, RuntimeConfig, PIPELINE_INGEST_WARNING_BUDGET_NS,
};

use crate::exchange_gateway::{ExchangeGatewayPort, GatewayBusinessHealthStatus};
use crate::worker_metrics::{prometheus_label, worker_metrics_path, write_worker_metrics};

/// 一个 worker 进程自启动以来的 pipeline 计数累计量，以及它落盘的那份 `.prom`。
#[derive(Default)]
pub struct PipelineMetricsReporter {
    worker_id: String,
    account_log: String,
    path: PathBuf,
    ingest_attempts: u64,
    ingested_events: u64,
    deduplicated_events: u64,
    transient_retries: u64,
    refreshes: u64,
    failures: u64,
    ingest_duration_ns_total: u64,
    ingest_duration_ns_max: u64,
    ingest_budget_warning_emitted: bool,
    last_observed_pipelines: BTreeMap<String, PipelineMetricsSnapshot>,
    gateway_business_status: BTreeMap<ExchangeGatewayPort, (GatewayBusinessHealthStatus, u64)>,
    last_published_ms: Option<u64>,
}

impl PipelineMetricsReporter {
    pub fn new(config: &RuntimeConfig, worker_id: &str, account_log: &str) -> Self {
        Self {
            worker_id: worker_id.into(),
            account_log: account_log.into(),
            path: worker_metrics_path(config, worker_id),
            ..Default::default()
        }
    }

    /// 把这个 pipeline 对象自打开起的计数并进 worker 的累计量。
    ///
    /// 调用点必须是「这个对象后面不再用了」的那一刻：同一个对象并两次就是双计，而并漏一个对象
    /// 就是少计，两边都不会让正文里的任何一行报错。
    pub fn absorb(&mut self, pipeline: &LiveEventPipeline) {
        let metrics = pipeline.metrics();
        self.ingest_attempts = self.ingest_attempts.saturating_add(metrics.ingest_attempts);
        self.ingested_events = self.ingested_events.saturating_add(metrics.ingested_events);
        self.deduplicated_events = self
            .deduplicated_events
            .saturating_add(metrics.deduplicated_events);
        self.transient_retries = self
            .transient_retries
            .saturating_add(metrics.transient_retries);
        self.refreshes = self.refreshes.saturating_add(metrics.refreshes);
        self.failures = self.failures.saturating_add(metrics.failures);
        self.ingest_duration_ns_total = self
            .ingest_duration_ns_total
            .saturating_add(metrics.ingest_duration_ns_total);
        self.ingest_duration_ns_max = self
            .ingest_duration_ns_max
            .max(metrics.ingest_duration_ns_max);
        self.warn_if_over_budget();
    }

    /// Fold a live pipeline's counter deltas into the worker-lifetime total.
    pub fn observe(&mut self, pipeline: &LiveEventPipeline) {
        self.observe_named("event-log", pipeline);
    }

    /// Fold one live pipeline into the worker total while keeping its own delta cursor.
    pub fn observe_named(&mut self, name: &str, pipeline: &LiveEventPipeline) {
        let current = pipeline.metrics();
        let previous = self
            .last_observed_pipelines
            .insert(name.into(), current.clone());
        self.ingest_attempts =
            self.ingest_attempts
                .saturating_add(metric_delta(&current, previous.as_ref(), |m| {
                    m.ingest_attempts
                }));
        self.ingested_events =
            self.ingested_events
                .saturating_add(metric_delta(&current, previous.as_ref(), |m| {
                    m.ingested_events
                }));
        self.deduplicated_events = self.deduplicated_events.saturating_add(metric_delta(
            &current,
            previous.as_ref(),
            |m| m.deduplicated_events,
        ));
        self.transient_retries =
            self.transient_retries
                .saturating_add(metric_delta(&current, previous.as_ref(), |m| {
                    m.transient_retries
                }));
        self.refreshes =
            self.refreshes
                .saturating_add(metric_delta(&current, previous.as_ref(), |m| m.refreshes));
        self.failures =
            self.failures
                .saturating_add(metric_delta(&current, previous.as_ref(), |m| m.failures));
        self.ingest_duration_ns_total = self.ingest_duration_ns_total.saturating_add(metric_delta(
            &current,
            previous.as_ref(),
            |m| m.ingest_duration_ns_total,
        ));
        self.ingest_duration_ns_max = self
            .ingest_duration_ns_max
            .max(current.ingest_duration_ns_max);
        self.warn_if_over_budget();
    }

    /// Record a port-specific business observation; process heartbeats remain a separate signal.
    pub(crate) fn observe_gateway_port_business_status(
        &mut self,
        port: ExchangeGatewayPort,
        status: GatewayBusinessHealthStatus,
        now_ms: u64,
    ) {
        if status.metric_value().is_some() {
            self.gateway_business_status
                .insert(port, (status, now_ms / 1_000));
        }
    }

    fn warn_if_over_budget(&mut self) {
        if !self.ingest_budget_warning_emitted
            && self.ingest_duration_ns_max > PIPELINE_INGEST_WARNING_BUDGET_NS
        {
            self.ingest_budget_warning_emitted = true;
            eprintln!(
                "[qx-service · Pipeline] worker={} ingest 单次耗时 {:.3}ms 超过 {:.0}ms 预算",
                self.worker_id,
                self.ingest_duration_ns_max as f64 / 1_000_000.0,
                PIPELINE_INGEST_WARNING_BUDGET_NS as f64 / 1_000_000.0,
            );
        }
    }

    /// High-volume stream workers publish at most once per second.
    pub fn publish_if_due(&mut self, up: bool, now_ms: u64) {
        const PUBLISH_INTERVAL_MS: u64 = 1_000;
        if self
            .last_published_ms
            .is_none_or(|last| now_ms.saturating_sub(last) >= PUBLISH_INTERVAL_MS)
        {
            self.publish(up, now_ms);
            self.last_published_ms = Some(now_ms);
        }
    }

    /// 落盘一次。写失败只播报、不改业务：指标出口坏了不该让已经撮合完的订单回滚。
    pub fn publish(&self, up: bool, now_ms: u64) {
        if let Err(error) = write_worker_metrics(&self.path, &self.body(up, now_ms)) {
            eprintln!(
                "worker={} pipeline 指标写入失败，业务处理继续: {}",
                self.worker_id, error
            );
        }
    }

    /// Prometheus 0.0.4 文本：逐行、`\n` 是真正的换行（#177 那条判据盯的就是这里）。
    ///
    /// 不带 `# HELP`/`# TYPE`，与 `worker-metrics/*.prom` 已有的那份正文同形态：抓取端按
    /// 样本名与值即可解析，`read_worker_metrics` 也是原样透传、只改写 `qx_worker_up` 那一行。
    fn body(&self, up: bool, now_ms: u64) -> String {
        let label = format!(
            "worker=\"{}\",account=\"{}\"",
            prometheus_label(&self.worker_id),
            prometheus_label(&self.account_log)
        );
        let mut body = format!(
            "qx_worker_up{{{label}}} {}\n\
qx_worker_heartbeat_timestamp_seconds{{{label}}} {}\n\
qx_pipeline_ingest_attempts_total{{{label}}} {}\n\
qx_pipeline_ingested_events_total{{{label}}} {}\n\
qx_pipeline_deduplicated_events_total{{{label}}} {}\n\
qx_pipeline_transient_retries_total{{{label}}} {}\n\
qx_pipeline_refreshes_total{{{label}}} {}\n\
qx_pipeline_failures_total{{{label}}} {}\n\
qx_pipeline_ingest_duration_seconds_total{{{label}}} {:.9}\n\
qx_pipeline_ingest_duration_seconds_max{{{label}}} {:.9}\n\
qx_pipeline_ingest_warning_budget_seconds{{{label}}} {:.9}\n\
qx_pipeline_ingest_budget_exceeded{{{label}}} {}\n",
            u8::from(up),
            now_ms / 1_000,
            self.ingest_attempts,
            self.ingested_events,
            self.deduplicated_events,
            self.transient_retries,
            self.refreshes,
            self.failures,
            self.ingest_duration_ns_total as f64 / 1_000_000_000.0,
            self.ingest_duration_ns_max as f64 / 1_000_000_000.0,
            PIPELINE_INGEST_WARNING_BUDGET_NS as f64 / 1_000_000_000.0,
            u8::from(self.ingest_duration_ns_max > PIPELINE_INGEST_WARNING_BUDGET_NS),
        );
        for (port, (status, timestamp_seconds)) in &self.gateway_business_status {
            let port_label = port.metric_label();
            let value = status
                .metric_value()
                .expect("unobserved business status is not persisted as a probe");
            body.push_str(&format!(
                "qx_gateway_port_business_status{{{label},port=\"{port_label}\"}} {value}\n\
qx_gateway_port_business_timestamp_seconds{{{label},port=\"{port_label}\"}} {timestamp_seconds}\n"
            ));
        }
        body
    }
}

fn metric_delta(
    current: &PipelineMetricsSnapshot,
    previous: Option<&PipelineMetricsSnapshot>,
    read: impl Fn(&PipelineMetricsSnapshot) -> u64,
) -> u64 {
    read(current).saturating_sub(previous.map_or(0, read))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway_execution_probe::GatewayExecutionProbeVenue;
    use qx_core::{InstrumentId, Order, OrderStatus, Price, Quantity, QxError, Side};
    use qx_zhenlu::{Venue, VenueEvent, VenueOrderSnapshot};

    #[test]
    fn worker_metrics_publish_gateway_business_status_separately_from_process_health() {
        let config = RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .expect("paper runtime config is valid");
        let mut reporter = PipelineMetricsReporter::new(&config, "market-data", "");
        reporter.observe_gateway_port_business_status(
            ExchangeGatewayPort::MarketData,
            GatewayBusinessHealthStatus::Healthy,
            91_000,
        );
        reporter.observe_gateway_port_business_status(
            ExchangeGatewayPort::AccountQuery,
            GatewayBusinessHealthStatus::Healthy,
            91_000,
        );
        reporter.observe_gateway_port_business_status(
            ExchangeGatewayPort::Reconciliation,
            GatewayBusinessHealthStatus::Degraded,
            91_000,
        );

        let body = reporter.body(true, 92_000);
        assert!(body.contains("qx_worker_up{worker=\"market-data\",account=\"\"} 1"));
        assert!(body.contains(
            "qx_gateway_port_business_status{worker=\"market-data\",account=\"\",port=\"market_data\"} 2"
        ));
        assert!(body.contains(
            "qx_gateway_port_business_timestamp_seconds{worker=\"market-data\",account=\"\",port=\"market_data\"} 91"
        ));
        assert!(body.contains(
            "qx_gateway_port_business_status{worker=\"market-data\",account=\"\",port=\"account_query\"} 2"
        ));
        assert!(body.contains(
            "qx_gateway_port_business_status{worker=\"market-data\",account=\"\",port=\"reconciliation\"} 1"
        ));
        assert!(!body.contains("port=\"venue_execution\""));
    }

    struct ProbeVenue {
        submit_result: Result<Vec<VenueEvent>, QxError>,
    }

    impl Venue for ProbeVenue {
        fn id(&self) -> &str {
            "BINANCE"
        }

        fn capabilities(&self) -> &'static qx_zhenlu::venue_capabilities::VenueCapabilities {
            &qx_zhenlu::venue_capabilities::BINANCE_SPOT_VENUE_CAPABILITIES
        }

        fn submit(&mut self, _order: Order, _ts: u64) -> Result<Vec<VenueEvent>, QxError> {
            self.submit_result.clone()
        }

        fn cancel(&mut self, _client_order_id: u64, _ts: u64) -> Result<Vec<VenueEvent>, QxError> {
            self.submit_result.clone()
        }

        fn snapshot(&self) -> Vec<VenueOrderSnapshot> {
            Vec::new()
        }

        fn connected(&self) -> bool {
            true
        }
    }

    fn probe_order() -> Order {
        Order {
            client_id: 1,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").expect("valid instrument"),
            side: Side::Buy,
            qty: Quantity::from_raw(1_000_000_000),
            limit: Some(Price::from_raw(50_000_000_000)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "alpha".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        }
    }

    #[test]
    fn venue_submit_boundary_writes_connector_result_status_to_gateway_metrics() {
        let config = RuntimeConfig::from_json(include_str!(
            "../../../deploy/qianxing.runtime.paper-strategy.example.json"
        ))
        .expect("paper runtime config is valid");
        let mut reporter = PipelineMetricsReporter::new(&config, "binance-exec", "alpha");
        let venue = ProbeVenue {
            submit_result: Ok(vec![VenueEvent::Accepted {
                client_order_id: 1,
                venue_order_id: "remote-1".into(),
                ts: 91_000,
            }]),
        };
        let mut venue = GatewayExecutionProbeVenue::new(venue, &mut reporter);
        venue
            .submit(probe_order(), 91_000)
            .expect("connector accepted order");
        drop(venue);
        let body = reporter.body(true, 92_000);
        assert!(body.contains(
            "qx_gateway_port_business_status{worker=\"binance-exec\",account=\"alpha\",port=\"venue_execution\"} 2"
        ));
    }
}
