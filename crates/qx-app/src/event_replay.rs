//! Application-owned verification for persisted EventLog report artifacts.

use crate::{AppError, AppErrorCategory};
use qx_core::Ledger;
use serde_json::Value;

/// Incremental replay verified against the persisted digest and ledger entries.
pub struct VerifiedEventReplayV1 {
    pub ledger: Ledger,
    pub event_count: u64,
    pub digest: String,
}

/// Parse and replay events, then reconcile both durable replay facts before projection.
pub fn verify_event_replay_v1(
    events: &[Value],
    expected_digest: &str,
    expected_ledger_entries: &Value,
) -> Result<VerifiedEventReplayV1, AppError> {
    let event_count =
        u64::try_from(events.len()).map_err(|_| invalid("EventLog 事件数超出 u64 范围"))?;
    let mut replay = qx_core::StreamingReplayVerifier::new(events.len());
    for (index, value) in events.iter().enumerate() {
        let event = serde_json::from_value(value.clone())
            .map_err(|error| invalid(format!("EventLog 第 {} 个事件无效: {error}", index + 1)))?;
        replay
            .append(event)
            .map_err(|error| invalid(format!("EventLog 重放失败: {error:?}")))?;
    }
    let replayed = replay
        .finish()
        .map_err(|error| invalid(format!("EventLog 重放结果校验失败: {error:?}")))?;
    let digest = format!("{:016x}", replayed.log_digest);
    let ledger_entries = serde_json::to_value(replayed.ledger.entries())
        .map_err(|error| invalid(format!("重放账簿序列化失败: {error}")))?;
    if digest != expected_digest || ledger_entries != *expected_ledger_entries {
        return Err(invalid("EventLog digest 或重放账簿与持久化产物不一致"));
    }
    Ok(VerifiedEventReplayV1 {
        ledger: replayed.ledger,
        event_count,
        digest,
    })
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::Conflict, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_verifies_digest_and_ledger_and_rejects_mismatches() {
        let expected_digest = format!("{:016x}", qx_core::EventLog::new().digest());
        let expected_entries = serde_json::to_value(Ledger::new().entries()).unwrap();
        let verified = verify_event_replay_v1(&[], &expected_digest, &expected_entries)
            .expect("empty event log and ledger reconcile");
        assert_eq!(verified.event_count, 0);
        assert_eq!(verified.digest, expected_digest);
        assert_eq!(verified.ledger.entries(), Ledger::new().entries());
        assert!(verify_event_replay_v1(&[], "wrong", &expected_entries).is_err());
        assert!(verify_event_replay_v1(&[], &expected_digest, &Value::Null).is_err());
        assert!(verify_event_replay_v1(
            &[serde_json::json!({})],
            &expected_digest,
            &expected_entries
        )
        .is_err());
    }

    #[test]
    fn replay_of_json_rows_matches_the_existing_eventlog_digest() {
        let mut event_log = qx_core::EventLog::new();
        event_log
            .append_checked(qx_core::Event::new(
                0,
                10,
                qx_core::Priority::APPLY,
                qx_core::EventKind::Settle,
            ))
            .unwrap();
        event_log
            .append_checked(qx_core::Event::new(
                1,
                20,
                qx_core::Priority::APPLY,
                qx_core::EventKind::Settle,
            ))
            .unwrap();
        let events = event_log
            .events()
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let expected_entries = serde_json::to_value(Ledger::new().entries()).unwrap();

        let verified = verify_event_replay_v1(
            &events,
            &format!("{:016x}", event_log.digest()),
            &expected_entries,
        )
        .expect("incremental replay accepts the existing EventLog digest");
        assert_eq!(verified.event_count, 2);
        assert_eq!(verified.digest, format!("{:016x}", event_log.digest()));
    }
}
