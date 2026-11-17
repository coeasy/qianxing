use qx_core::{EventLog, Ledger, QxError, ReplayVerifier};

/// Refuse a report unless its event facts rebuild the ledger being returned with it.
/// Both Bar and order-book engines use this as their final report-return boundary.
pub(crate) fn validate_before_return<T>(
    report: T,
    facts: impl for<'a> FnOnce(&'a T) -> (&'a EventLog, &'a Ledger),
) -> Result<T, QxError> {
    let (event_log, ledger) = facts(&report);
    ReplayVerifier::verify(event_log.events(), ledger)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::validate_before_return;
    use qx_core::{Event, EventKind, EventLog, Ledger, Money};

    struct ReportFacts {
        event_log: EventLog,
        ledger: Ledger,
    }

    fn finish(report: ReportFacts) -> Result<ReportFacts, qx_core::QxError> {
        validate_before_return(report, |report| (&report.event_log, &report.ledger))
    }

    #[test]
    fn report_exit_rejects_an_event_log_with_duplicate_sequence_numbers() {
        let mut event_log = EventLog::new();
        event_log.append(Event::new(0, 10, 2, EventKind::Settle));
        event_log.append(Event::new(0, 20, 2, EventKind::Settle));

        let error = finish(ReportFacts {
            event_log,
            ledger: Ledger::new(),
        })
        .err()
        .expect("an invalid event log must not escape as a report");

        assert!(error.to_string().contains("重复"), "{error}");
    }

    #[test]
    fn report_exit_rejects_a_ledger_entry_without_a_fact_event() {
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "USD", Money::from_i64(100), 10)
            .expect("test deposit is valid");

        let error = finish(ReportFacts {
            event_log: EventLog::new(),
            ledger,
        })
        .err()
        .expect("a ledger not represented in the event log must not escape");

        assert!(error.to_string().contains("账簿不一致"), "{error}");
    }

    #[test]
    fn report_exit_returns_a_report_when_replay_matches() {
        let mut event_log = EventLog::new();
        event_log.append(Event::new(0, 10, 2, EventKind::Settle));

        let report = finish(ReportFacts {
            event_log,
            ledger: Ledger::new(),
        })
        .expect("valid report facts are returned");

        assert_eq!(report.event_log.len(), 1);
    }
}
