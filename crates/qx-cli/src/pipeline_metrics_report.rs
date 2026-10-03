//! Paper worker 的 pipeline 计数出口（V13 §9.27 #178）。
//!
//! `LiveEventPipeline` 的六个计数（`ingest_attempts`/`ingested_events`/`deduplicated_events`/
//! `transient_retries`/`refreshes`/`failures`）在 `ingest`/`refresh` 里每笔都在加，可第八遍立案时
//! 全仓对 `metrics()` 的调用只有那个 crate 自己的用例 —— 数据在生产里被算出来再丢掉。当时不接线
//! 是因为量纲未定：计数按 pipeline 对象自打开起累计，而 paper 循环里行情按命令、恢复按 tick 各开
//! 一个对象，直接印成 `_total` 就是给抓取端一条每次请求归零的「累计计数」，比不印更糟。
//!
//! 这里把量纲定成**按 worker 进程**：每个 pipeline 用完（不再写它）时把它的计数并进累计量，每轮
//! 把累计量写进 `worker-metrics/<worker_id>.prom`。归零的边界因此收在进程重启这一处，与
//! `qx_worker_up`、心跳同一口径，也正是抓取端对 counter 重启的标准处理。
//!
//! 不在这个出口里计的站点逐名登记在 `tests/pipeline_metrics_open_sites.rs` 的豁免名单里，并由那条
//! 判据与本段双向核对 —— 名单改了文档不改、或者反过来，都会红：
//! `open_account_pipeline`（统一入口向 `open_runtime_pipeline` 的委托，不是新的打开点）、
//! `build_configured_api_service`、`load_api_query_models`、`load_api_account_snapshot_for_worker`、
//! `strategy_current_qty_for`、`strategy_account_context`（服务与读模型按请求各开一个只读 pipeline，
//! 存活期与「按 worker 进程累计」不符，接它要另立一份跨请求的身份），以及
//! `run_paper_pipeline_once`、`run_paper_submit_order` 与它拆出的 `paper_submit_action`
//! （一次性验收入口，不是长驻 worker：
//! 给它们落 `.prom` 等于替一个已经退出的进程宣称健康。它们写入的事实照旧进 EventLog，
//! 只是不在这族样本里 —— `paper-submit-order` 那条还会以 `paper-execution` 的名义领取租约，
//! 所以它的计数并进 worker 会更假）。

use super::*;

/// 一个 worker 进程自启动以来的 pipeline 计数累计量，以及它落盘的那份 `.prom`。
#[derive(Default)]
pub(crate) struct PipelineMetricsReporter {
    worker_id: String,
    account_log: String,
    path: PathBuf,
    ingest_attempts: u64,
    ingested_events: u64,
    deduplicated_events: u64,
    transient_retries: u64,
    refreshes: u64,
    failures: u64,
}

impl PipelineMetricsReporter {
    pub(crate) fn new(config: &RuntimeConfig, worker_id: &str, account_log: &str) -> Self {
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
    pub(crate) fn absorb(&mut self, pipeline: &LiveEventPipeline) {
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
    }

    /// 落盘一次。写失败只播报、不改业务：指标出口坏了不该让已经撮合完的订单回滚。
    pub(crate) fn publish(&self, up: bool, now_ms: u64) {
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
        format!(
            "qx_worker_up{{{label}}} {}\n\
qx_worker_heartbeat_timestamp_seconds{{{label}}} {}\n\
qx_pipeline_ingest_attempts_total{{{label}}} {}\n\
qx_pipeline_ingested_events_total{{{label}}} {}\n\
qx_pipeline_deduplicated_events_total{{{label}}} {}\n\
qx_pipeline_transient_retries_total{{{label}}} {}\n\
qx_pipeline_refreshes_total{{{label}}} {}\n\
qx_pipeline_failures_total{{{label}}} {}\n",
            u8::from(up),
            now_ms / 1_000,
            self.ingest_attempts,
            self.ingested_events,
            self.deduplicated_events,
            self.transient_retries,
            self.refreshes,
            self.failures,
        )
    }
}
