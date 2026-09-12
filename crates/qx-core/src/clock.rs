//! 时钟：确定性时间源。
//!
//! 回测只使用 [`TestClock`]；时间只在 `advance_to` 时前进，
//! 任何读取系统时间的行为都会破坏可重放性。

/// 纳秒时间戳。
pub type Ts = u64;

pub const NANOS_PER_SEC: u64 = 1_000_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClockError {
    Backward,
}

/// 确定性时钟：仅在显式推进时前进。
#[derive(Clone, Debug, Default)]
pub struct TestClock {
    now: Ts,
}

impl TestClock {
    pub fn new(start: Ts) -> Self {
        Self { now: start }
    }

    pub fn now(&self) -> Ts {
        self.now
    }

    /// 单调推进到 `t`；回退会被拒绝（时间倒流是严重错误，不是"容错"）。
    pub fn advance_to(&mut self, t: Ts) -> Result<(), ClockError> {
        if t < self.now {
            return Err(ClockError::Backward);
        }
        self.now = t;
        Ok(())
    }

    pub fn advance_by(&mut self, d: Ts) {
        self.now += d;
    }
}

/// 一天中的纳秒数，用于日历推进。
pub const NANOS_PER_DAY: u64 = 86_400 * NANOS_PER_SEC;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_is_monotonic() {
        let mut c = TestClock::new(100);
        assert!(c.advance_to(50).is_err());
        assert_eq!(c.now(), 100);
        c.advance_to(200).unwrap();
        assert_eq!(c.now(), 200);
    }
}
