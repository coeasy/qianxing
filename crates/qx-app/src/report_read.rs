//! Versioned, bounded core read model for verified research runs.
//!
//! HTTP, CLI, Python and SDK report surfaces may add presentation-specific drilldowns, but
//! the identity, verification verdict and bounded equity/fill samples come from this use case.
//! Stable serialized DTOs live in `model`; this module reads and verifies persisted artifacts
//! before assembling those models.

use crate::artifact_store::RunRecordArtifactLocators;
use crate::artifact_store::{
    artifact_too_large, read_bounded_line, read_text_bounded, read_text_bounded_by_id, ArtifactId,
    ArtifactLocatorV1, ArtifactReader, ArtifactStream, FileArtifactReader, MAX_ARTIFACT_ROWS,
};
use crate::cases::{
    verify_depth_run_with_reader, verify_run_with_reader, ProviderBacktestOutcome,
    ProviderDepthBacktestOutcome,
};
use crate::{
    AppError, AppErrorCategory, BacktestOutcome, CallerCapability, DepthBacktestOutcome,
    MultiMarketBacktestOutcome, MultiMarketBacktestSpec, MultiMarketDepthBacktestOutcome,
    MultiMarketDepthBacktestSpec, RunContext, StrategyArtifactIdentity, VerificationResult,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;

mod artifact_views;
mod model;
mod single_event;
pub use single_event::read_event_report_v1_with_reader;
mod standard;
pub use standard::{
    read_depth_run_report_v1, read_depth_run_report_v1_with_reader,
    read_provider_backtest_report_v1, read_provider_backtest_report_v1_with_reader,
    read_provider_depth_backtest_report_v1, read_provider_depth_backtest_report_v1_with_readers,
    read_run_report_v1, read_run_report_v1_with_reader,
};
mod multi_market_events;
pub use multi_market_events::{
    read_multi_market_depth_event_report_page_v1,
    read_multi_market_depth_event_report_page_v1_with_reader,
    read_multi_market_depth_event_report_read_model_v1_with_reader,
    read_multi_market_event_report_page_v1, read_multi_market_event_report_page_v1_with_reader,
    read_multi_market_event_report_read_model_v1_with_reader,
    read_multi_market_event_report_v1_with_reader, read_multi_market_legacy_event_report_v1,
};

pub use model::{
    EventFillCountV1, EventFillIdentityV1, EventReportPageV1,
    MultiMarketDepthEventReportReadModelV1, MultiMarketDepthEventReportReadRequestV1,
    MultiMarketEventReportPageRequestV1, MultiMarketEventReportPageV1,
    MultiMarketEventReportReadModelV1, MultiMarketEventReportReadRequestV1,
    ProviderBacktestReportV1, ProviderDepthBacktestReportV1, ResearchReportRowsV1,
    ResearchRunReportV1, VerifiedMultiMarketEventReportV1, EVENT_REPORT_PAGE_SCHEMA_VERSION,
    MULTI_MARKET_EVENT_REPORT_SCHEMA_VERSION_V1, PROVIDER_BACKTEST_REPORT_SCHEMA_VERSION_V1,
    PROVIDER_DEPTH_BACKTEST_REPORT_SCHEMA_VERSION_V1, RESEARCH_RUN_REPORT_SCHEMA_VERSION,
};

const MAX_ARTIFACT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SUMMARY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_EQUITY_ROWS: usize = 1_000;
const MAX_FILL_ROWS: usize = 200;
pub const MAX_EVENT_REPORT_PAGE_ROWS: usize = 300;

/// Build the market-bar context visible to a report event under the engine's causal ordering.
/// Strategy commands use a strict timestamp prefix, so the bar that triggered a decision is not
/// included in its completed-history context.
pub fn multi_market_bar_context(
    frame: &qx_datastruct::BarFrame,
    event_ts: u64,
    event_priority: u8,
) -> Option<Value> {
    let strict_prior =
        event_priority == qx_core::Priority::COMMAND || event_priority <= qx_core::Priority::MARKET;
    let visible_end = frame.ts.partition_point(|bar_ts| {
        if strict_prior {
            *bar_ts < event_ts
        } else {
            *bar_ts <= event_ts
        }
    });
    let bar_index = visible_end.checked_sub(1)?;
    let bar_ts = *frame.ts.get(bar_index)?;
    Some(serde_json::json!({
        "event_ts": event_ts,
        "bar_index": bar_index,
        "relation": if bar_ts == event_ts { "same_timestamp_visible" } else { "previous_visible" },
        "bar": {
            "ts": bar_ts,
            "open_raw": frame.open_raw.get(bar_index)?.to_string(),
            "high_raw": frame.high_raw.get(bar_index)?.to_string(),
            "low_raw": frame.low_raw.get(bar_index)?.to_string(),
            "close_raw": frame.close_raw.get(bar_index)?.to_string(),
            "volume_raw": frame.volume_raw.get(bar_index)?.to_string()
        }
    }))
}

pub(crate) fn update_fill_sequence_digest(
    digest: &mut qx_strategy::file_digest::Sha256Accumulator,
    identity: &EventFillIdentityV1,
) -> Result<(), AppError> {
    let encoded = serde_json::to_vec(identity).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("成交身份摘要序列化失败: {error}"),
        )
    })?;
    let len = u64::try_from(encoded.len()).map_err(|_| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            "成交身份摘要长度超出 u64",
        )
    })?;
    digest.update(&len.to_le_bytes());
    digest.update(&encoded);
    Ok(())
}

pub(crate) fn event_fill_identity(fill: &qx_core::Fill) -> EventFillIdentityV1 {
    EventFillIdentityV1 {
        ts: fill.ts,
        order_id: fill.order_id,
        qty_raw: fill.qty.raw(),
        price_raw: fill.price.raw(),
        fee_raw: fill.fee.raw(),
        strategy_id: fill.strategy_id.clone().unwrap_or_default(),
    }
}

/// Recover instrument attribution only for orders referenced by the requested event page.
///
/// The first pass already verifies replay and collects that bounded page. This second pass
/// builds a page-sized order index, then checks that the trace bytes did not change between
/// reads. The returned index therefore grows with the requested page, not the full run.
#[cfg(test)]
mod event_report_tests {
    use super::artifact_views::{read_csv_view, read_csv_view_by_id};
    use super::*;
    use crate::artifact_store::{ArtifactReader, ArtifactStream};
    use std::io::Cursor;

    struct BytesReader {
        event_log: Vec<u8>,
        manifest: Vec<u8>,
    }

    impl ArtifactReader for BytesReader {
        fn open(&self, reference: &str) -> Result<ArtifactStream, AppError> {
            let bytes = if reference == "memory:manifest" {
                self.manifest.clone()
            } else {
                self.event_log.clone()
            };
            Ok(ArtifactStream::new(bytes.len() as u64, Cursor::new(bytes)))
        }
    }

    fn empty_log_facts() -> (Value, Value, String) {
        let digest = format!("{:016x}", qx_core::EventLog::new().digest());
        let summary = serde_json::json!({
            "ledger_entries": 0,
            "replay_ledger_entries": 0,
            "replay": {
                "log_digest": digest,
                "events": 0,
                "ledger_entries": 0,
                "run_ledger_entries": 0
            }
        });
        let manifest = serde_json::json!({"output_event_hash": digest});
        (summary, manifest, digest)
    }

    fn memory_reader(manifest: &Value) -> BytesReader {
        BytesReader {
            event_log: Vec::new(),
            manifest: serde_json::to_vec(manifest).expect("serialize manifest fixture"),
        }
    }

    #[test]
    fn event_report_replays_and_returns_a_versioned_bounded_page() {
        let (summary, manifest, _) = empty_log_facts();
        let report = read_event_report_v1_with_reader(
            "memory:event-log",
            &summary,
            "memory:manifest",
            None,
            &memory_reader(&manifest),
        )
        .expect("empty replay-verified event log");
        assert_eq!(report.schema_version, EVENT_REPORT_PAGE_SCHEMA_VERSION);
        assert_eq!(report.total, 0);
        assert_eq!(report.offset, 0);
        assert!(!report.truncated);
        assert!(report.rows.is_empty());
        assert!(report.fill_counts.is_empty());
    }

    #[test]
    fn event_report_rejects_a_summary_digest_that_does_not_match_replay() {
        let (mut summary, manifest, digest) = empty_log_facts();
        summary["replay"]["log_digest"] = serde_json::Value::String(format!(
            "{:016x}",
            u64::from_str_radix(&digest, 16).unwrap() + 1
        ));
        let error = read_event_report_v1_with_reader(
            "memory:event-log",
            &summary,
            "memory:manifest",
            None,
            &memory_reader(&manifest),
        )
        .expect_err("a changed digest must fail closed");
        assert_eq!(error.category(), AppErrorCategory::StorageFailure);
    }

    #[test]
    fn event_report_rejects_a_page_larger_than_the_registered_limit() {
        let (summary, manifest, _) = empty_log_facts();
        let error = read_event_report_v1_with_reader(
            "memory:event-log",
            &summary,
            "memory:manifest",
            Some((0, MAX_EVENT_REPORT_PAGE_ROWS + 1)),
            &memory_reader(&manifest),
        )
        .expect_err("unbounded pages must be rejected");
        assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    }

    #[test]
    fn multi_market_order_index_keeps_only_order_ids_needed_by_the_page() {
        let submitted = |client_id, instrument: &str| {
            let order = qx_core::Order {
                client_id,
                instrument: qx_core::InstrumentId::parse(instrument)
                    .expect("valid fixture instrument"),
                side: qx_core::Side::Buy,
                qty: qx_core::Quantity::from_i64(1),
                limit: None,
                status: qx_core::OrderStatus::PendingSubmit,
                filled: qx_core::Quantity::from_i64(0),
                account_id: "account-a".into(),
                trace: None,
                policy: None,
                exact_amounts: None,
            };
            qx_core::Event::new(
                client_id,
                1,
                qx_core::Priority::MATCH,
                qx_core::EventKind::OrderSubmitted { order },
            )
        };
        let mut event_log = Vec::new();
        for event in [submitted(11, "MSFT.NASDAQ"), submitted(12, "NVDA.NASDAQ")] {
            serde_json::to_writer(&mut event_log, &event).expect("encode event fixture");
            event_log.push(b'\n');
        }
        let mut expected_digest = qx_strategy::file_digest::Sha256Accumulator::new();
        expected_digest.update(b"qx-multi-market-event-trace-bytes-v1\0");
        expected_digest.update(&event_log);
        let reader = BytesReader {
            event_log,
            manifest: Vec::new(),
        };
        let index = multi_market_events::read_multi_market_order_market_index_v1(
            "memory:event-log",
            &expected_digest.finish_hex(),
            &BTreeSet::from([12]),
            &BTreeMap::new(),
            &reader,
        )
        .expect("index should be limited to the requested page order IDs");
        assert_eq!(index, BTreeMap::from([(12, "NVDA.NASDAQ".to_owned())]));
        let changed_error = multi_market_events::read_multi_market_order_market_index_v1(
            "memory:event-log",
            &"0".repeat(64),
            &BTreeSet::from([12]),
            &BTreeMap::new(),
            &reader,
        )
        .expect_err("the second-pass bytes must match the replayed trace");
        assert!(changed_error.message().contains("分页归属复核期间发生变化"));
    }

    fn submitted_event(sequence: u64, client_id: u64, instrument: &str) -> qx_core::Event {
        let order = qx_core::Order {
            client_id,
            instrument: qx_core::InstrumentId::parse(instrument).expect("valid fixture instrument"),
            side: qx_core::Side::Buy,
            qty: qx_core::Quantity::from_i64(1),
            limit: None,
            status: qx_core::OrderStatus::PendingSubmit,
            filled: qx_core::Quantity::from_i64(0),
            account_id: "account-a".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        };
        qx_core::Event::new(
            sequence,
            sequence + 1,
            qx_core::Priority::MATCH,
            qx_core::EventKind::OrderSubmitted { order },
        )
    }

    #[test]
    fn legacy_multi_market_report_replays_inline_events_and_projects_a_bounded_page() {
        let mut log = qx_core::EventLog::new();
        let events = [
            submitted_event(0, 11, "MSFT.NASDAQ"),
            submitted_event(1, 12, "NVDA.NASDAQ"),
        ]
        .into_iter()
        .map(|event| {
            log.append_checked(event.clone())
                .expect("valid ordered EventLog event");
            serde_json::to_value(event).expect("serialize EventLog event")
        })
        .collect::<Vec<_>>();
        let ledger_entries =
            serde_json::to_value(qx_core::Ledger::new().entries()).expect("serialize empty ledger");
        let report = read_multi_market_legacy_event_report_v1(
            &events,
            &format!("{:016x}", log.digest()),
            &ledger_entries,
            &BTreeMap::new(),
            Some((1, 1)),
        )
        .expect("verify and project legacy event page");

        assert_eq!(report.total, 2);
        assert_eq!(report.offset, 1);
        assert!(report.truncated);
        assert_eq!(report.rows, vec![events[1].clone()]);
        assert_eq!(report.fill_count, 0);
        assert_eq!(
            report.order_markets,
            BTreeMap::from([
                (11, "MSFT.NASDAQ".to_owned()),
                (12, "NVDA.NASDAQ".to_owned()),
            ])
        );
    }

    #[test]
    fn legacy_multi_market_report_rejects_one_order_attributed_to_two_markets() {
        let mut log = qx_core::EventLog::new();
        let events = [
            submitted_event(0, 11, "MSFT.NASDAQ"),
            submitted_event(1, 11, "NVDA.NASDAQ"),
        ]
        .into_iter()
        .map(|event| {
            log.append_checked(event.clone())
                .expect("ordered events are valid for replay");
            serde_json::to_value(event).expect("serialize EventLog event")
        })
        .collect::<Vec<_>>();
        let ledger_entries =
            serde_json::to_value(qx_core::Ledger::new().entries()).expect("serialize empty ledger");
        let error = read_multi_market_legacy_event_report_v1(
            &events,
            &format!("{:016x}", log.digest()),
            &ledger_entries,
            &BTreeMap::new(),
            Some((0, 2)),
        )
        .expect_err("conflicting order market attribution must be rejected");

        assert!(error.message().contains("关联到不同标的"));
    }

    #[test]
    fn fills_report_keeps_the_first_rows_for_api_replay_projection_compatibility() {
        let reader = BytesReader {
            event_log: b"id,value\n1,a\n2,b\n3,c\n4,d\n5,e\n".to_vec(),
            manifest: Vec::new(),
        };
        let view = read_csv_view(&reader, "memory:fills", 2, false).expect("bounded fills view");
        assert_eq!(view.total, 5);
        assert!(view.truncated);
        assert_eq!(view.rows[0]["id"], "1");
        assert_eq!(view.rows[1]["id"], "2");
    }

    struct IdOnlyReader(Vec<u8>);

    impl ArtifactReader for IdOnlyReader {
        fn open(&self, _reference: &str) -> Result<ArtifactStream, AppError> {
            Err(AppError::new(
                AppErrorCategory::DataUnavailable,
                "path lookup is intentionally unavailable",
            ))
        }

        fn open_by_id(
            &self,
            _artifact_id: &ArtifactId,
            _legacy_reference: Option<&str>,
        ) -> Result<ArtifactStream, AppError> {
            Ok(ArtifactStream::new(
                self.0.len() as u64,
                Cursor::new(self.0.clone()),
            ))
        }
    }

    #[test]
    fn report_projection_checks_csv_content_id_during_the_sampling_read() {
        let expected = b"ts,equity_raw,position_raw\n1,10,0\n2,20,1\n";
        let id = ArtifactId::from_sha256_hex(qx_strategy::sha256_hex(expected))
            .expect("valid content digest");
        let reader = IdOnlyReader(b"ts,equity_raw,position_raw\n1,10,0\n2,21,1\n".to_vec());
        let locator = ArtifactLocatorV1 {
            id,
            legacy_reference: "legacy-equity.csv".to_owned(),
        };

        let error = read_csv_view_by_id(&reader, &locator, 10, true)
            .expect_err("projection must reject content that does not match its ID");
        assert_eq!(error.category(), AppErrorCategory::Conflict);
    }

    #[test]
    fn strategy_event_context_does_not_expose_the_bar_that_triggered_it() {
        let frame = qx_datastruct::BarFrame {
            instrument: qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            source: qx_guanxing::DataSourceId::new("fixture:causal-bar-context"),
            ts: vec![100, 200],
            open_raw: vec![10, 20],
            high_raw: vec![12, 22],
            low_raw: vec![9, 19],
            close_raw: vec![11, 21],
            volume_raw: vec![100, 200],
            exact_ohlcv: None,
        };
        let decision = multi_market_bar_context(&frame, 200, qx_core::Priority::COMMAND)
            .expect("a previous visible bar exists");
        assert_eq!(decision["bar_index"], 0);
        assert_eq!(decision["relation"], "previous_visible");
        assert_eq!(decision["bar"]["close_raw"], "11");

        let fill = multi_market_bar_context(&frame, 200, qx_core::Priority::APPLY)
            .expect("the current bar is visible after matching");
        assert_eq!(fill["bar_index"], 1);
        assert_eq!(fill["relation"], "same_timestamp_visible");
    }

    #[test]
    fn event_before_the_first_bar_has_no_market_context() {
        let frame = qx_datastruct::BarFrame {
            instrument: qx_core::InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            source: qx_guanxing::DataSourceId::new("fixture:empty-causal-prefix"),
            ts: vec![100],
            open_raw: vec![10],
            high_raw: vec![12],
            low_raw: vec![9],
            close_raw: vec![11],
            volume_raw: vec![100],
            exact_ohlcv: None,
        };
        assert!(multi_market_bar_context(&frame, 50, qx_core::Priority::COMMAND).is_none());
    }
}
