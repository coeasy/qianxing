//! NATS-backed outbox and event-consumer worker loops.
//!
//! The CLI owns configuration loading, backend construction, process handlers and metric files.
//! This module owns the runtime lifecycle shared by those adapters: polling, heartbeats, batches,
//! and stop-aware waits.

use crate::{lease_clock, MessagingRuntimeConfig, WorkerContext};
use qx_storage::{
    ConsumerStateStore, NatsConsumerBatchReport, NatsJetStreamConsumer, NatsJetStreamPublisher,
    OutboxEvent, OutboxRelay, OutboxRelayReport, OutboxStore,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Default)]
pub struct RelayMetricTotals {
    scanned: u64,
    published: u64,
    retried: u64,
    lease_conflicts: u64,
    publish_failures: u64,
    parked: u64,
}

impl RelayMetricTotals {
    pub fn apply(&mut self, report: &OutboxRelayReport) {
        self.scanned += report.scanned;
        self.published += report.published;
        self.retried += report.retried;
        self.lease_conflicts += report.lease_conflicts;
        self.publish_failures += report.publish_failures;
        // parked describes the current queue state, so keep the latest observation.
        self.parked = report.parked;
    }

    pub fn render(&self, escaped_worker_label: &str, up: bool, now_ms: u64) -> String {
        format!(
            "qx_worker_up{{worker=\"{escaped_worker_label}\"}} {}\n\
qx_worker_heartbeat_timestamp_seconds{{worker=\"{escaped_worker_label}\"}} {}\n\
qx_outbox_relay_scanned_total{{worker=\"{escaped_worker_label}\"}} {}\n\
qx_outbox_relay_published_total{{worker=\"{escaped_worker_label}\"}} {}\n\
qx_outbox_relay_retried_total{{worker=\"{escaped_worker_label}\"}} {}\n\
qx_outbox_relay_lease_conflicts_total{{worker=\"{escaped_worker_label}\"}} {}\n\
qx_outbox_relay_publish_failures_total{{worker=\"{escaped_worker_label}\"}} {}\n\
qx_outbox_relay_parked{{worker=\"{escaped_worker_label}\"}} {}\n",
            u8::from(up),
            now_ms / 1_000,
            self.scanned,
            self.published,
            self.retried,
            self.lease_conflicts,
            self.publish_failures,
            self.parked,
        )
    }
}

#[derive(Default)]
pub struct ConsumerMetricTotals {
    received: u64,
    applied: u64,
    duplicates: u64,
    retried: u64,
    dead_lettered: u64,
    malformed: u64,
    ack_failures: u64,
}

impl ConsumerMetricTotals {
    pub fn apply(&mut self, report: &NatsConsumerBatchReport) {
        self.received += report.received;
        self.applied += report.applied;
        self.duplicates += report.duplicates;
        self.retried += report.retried;
        self.dead_lettered += report.dead_lettered;
        self.malformed += report.malformed;
        self.ack_failures += report.ack_failures;
    }

    pub fn render(&self, escaped_worker_label: &str, up: bool, now_ms: u64) -> String {
        format!(
            "qx_worker_up{{worker=\"{escaped_worker_label}\"}} {}\n\
qx_worker_heartbeat_timestamp_seconds{{worker=\"{escaped_worker_label}\"}} {}\n\
qx_event_consumer_received_total{{worker=\"{escaped_worker_label}\"}} {}\n\
qx_event_consumer_applied_total{{worker=\"{escaped_worker_label}\"}} {}\n\
qx_event_consumer_duplicates_total{{worker=\"{escaped_worker_label}\"}} {}\n\
qx_event_consumer_retried_total{{worker=\"{escaped_worker_label}\"}} {}\n\
qx_event_consumer_dead_lettered_total{{worker=\"{escaped_worker_label}\"}} {}\n\
qx_event_consumer_malformed_total{{worker=\"{escaped_worker_label}\"}} {}\n\
qx_event_consumer_ack_failures_total{{worker=\"{escaped_worker_label}\"}} {}\n",
            u8::from(up),
            now_ms / 1_000,
            self.received,
            self.applied,
            self.duplicates,
            self.retried,
            self.dead_lettered,
            self.malformed,
            self.ack_failures,
        )
    }
}

pub fn run_outbox_relay_loop<S, M>(
    context: WorkerContext,
    relay: OutboxRelay<S, NatsJetStreamPublisher>,
    messaging: MessagingRuntimeConfig,
    once: bool,
    mut publish_metrics: M,
) -> Result<(), String>
where
    S: OutboxStore + 'static,
    M: FnMut(Option<&OutboxRelayReport>, bool, u64),
{
    publish_metrics(None, true, runtime_timestamp_ms());
    loop {
        if context.should_stop() {
            break;
        }
        let now = runtime_timestamp_ms();
        let report = relay
            .pump_once(lease_clock(now), messaging.relay_batch_size)
            .map_err(|error| format!("Outbox relay 批次失败: {error:?}"))?;
        publish_metrics(Some(&report), true, now);
        context.heartbeat(now)?;
        println!(
            "[Outbox relay worker={}] scanned={} published={} retried={} failures={} conflicts={} parked={} last_error={:?}",
            context.id(),
            report.scanned,
            report.published,
            report.retried,
            report.publish_failures,
            report.lease_conflicts,
            report.parked,
            report.last_error
        );
        if once {
            break;
        }
        wait_for_worker_interval(&context, messaging.relay_interval_ms);
    }
    publish_metrics(None, false, runtime_timestamp_ms());
    Ok(())
}

pub fn run_event_consumer_loop<S, H, M>(
    context: WorkerContext,
    consumer: NatsJetStreamConsumer,
    store: S,
    messaging: MessagingRuntimeConfig,
    handler: H,
    once: bool,
    mut publish_metrics: M,
) -> Result<(), String>
where
    S: ConsumerStateStore + Clone + Send + Sync + 'static,
    H: Fn(&OutboxEvent) -> Result<(), String> + Clone + Send + Sync + 'static,
    M: FnMut(Option<&NatsConsumerBatchReport>, bool, u64),
{
    publish_metrics(None, true, runtime_timestamp_ms());
    loop {
        if context.should_stop() {
            break;
        }
        let handler = handler.clone();
        let report = consumer.consume_batch(
            store.clone(),
            NatsJetStreamConsumer::current_timestamp(),
            messaging.consumer_batch_size,
            move |event| handler(event),
        )?;
        let now = runtime_timestamp_ms();
        publish_metrics(Some(&report), true, now);
        context.heartbeat(now)?;
        println!(
            "[Event consumer worker={}] received={} applied={} duplicates={} retried={} dead_lettered={} malformed={} ack_failures={} last_error={:?}",
            context.id(),
            report.received,
            report.applied,
            report.duplicates,
            report.retried,
            report.dead_lettered,
            report.malformed,
            report.ack_failures,
            report.last_error
        );
        if once {
            break;
        }
        wait_for_worker_interval(&context, messaging.relay_interval_ms);
    }
    publish_metrics(None, false, runtime_timestamp_ms());
    Ok(())
}

fn wait_for_worker_interval(context: &WorkerContext, interval_ms: u64) {
    let deadline = std::time::Instant::now() + Duration::from_millis(interval_ms);
    while !context.should_stop() {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}

fn runtime_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
