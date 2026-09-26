use super::*;

#[test]
fn ccxt_stream_backoff_escalates_and_resets_on_a_healthy_session() {
    let mut budget = CcxtStreamReconnectBudget::default();
    assert_eq!(budget.note_failure().unwrap(), Duration::from_millis(500));
    assert_eq!(budget.note_failure().unwrap(), Duration::from_millis(1_000));
    assert_eq!(budget.note_failure().unwrap(), Duration::from_millis(2_000));
    assert_eq!(budget.consecutive_failures(), 3);
    budget.note_success();
    assert_eq!(budget.consecutive_failures(), 0);
    // 清零后退避从第一档重新爬，不会记住健康会话之前的失败。
    assert_eq!(budget.note_failure().unwrap(), Duration::from_millis(500));
}

#[test]
fn ccxt_stream_reconnect_budget_gives_up_after_consecutive_failures() {
    let mut budget = CcxtStreamReconnectBudget::default();
    let mut last_delay = Duration::ZERO;
    for attempt in 1..=CcxtStreamReconnectBudget::MAX_RECONNECTS {
        last_delay = budget
            .note_failure()
            .unwrap_or_else(|error| unreachable!("第 {attempt} 次重连不该放弃: {error}"));
    }
    assert_eq!(
        last_delay,
        Duration::from_secs(8),
        "退避应在 max_delay 处封顶"
    );
    match budget.note_failure() {
        Ok(delay) => unreachable!("超过上限后不该再等待 {delay:?}"),
        Err(error) => assert!(
            error.contains(&format!(
                "连续 {} 次",
                CcxtStreamReconnectBudget::MAX_RECONNECTS + 1
            )),
            "{error}"
        ),
    }
}
