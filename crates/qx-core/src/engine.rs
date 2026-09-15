//! 引擎：确定性事件循环。
//!
//! 关键约束：
//! - 时间只在 `advance_to` 时前进，绝不读系统时间。
//! - 每次 handler 执行后重新检查队列（定时器可生成新定时器，命令可触发新事件），
//!   因此**不能预先展开全部时间轴**。

use crate::clock::{TestClock, Ts};
use crate::error::{QxError, QxResult};
use crate::event::{Event, EventKind};
use crate::queue::CausalQueue;
use crate::sourcing::EventLog;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub struct EngineCtx {
    pub clock: TestClock,
    pub queue: CausalQueue,
    pub log: EventLog,
    next_seq: u64,
    causal_depths: BTreeMap<u64, usize>,
    current_event: Option<(u64, usize)>,
}

impl EngineCtx {
    pub fn new(start: Ts) -> Self {
        Self {
            clock: TestClock::new(start),
            queue: CausalQueue::new(),
            log: EventLog::new(),
            next_seq: 0,
            causal_depths: BTreeMap::new(),
            current_event: None,
        }
    }

    pub fn now(&self) -> Ts {
        self.clock.now()
    }

    /// 在指定时刻投递事件，返回 seq。
    pub fn emit_at(&mut self, ts: Ts, prio: u8, kind: EventKind) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        let mut event = Event::new(seq, ts, prio, kind);
        let depth = if let Some((parent_seq, parent_depth)) = self.current_event {
            event = event.caused_by(parent_seq);
            parent_depth.saturating_add(1)
        } else {
            0
        };
        self.causal_depths.insert(seq, depth);
        self.queue.push(event);
        seq
    }

    /// 在当前时刻投递事件。
    pub fn emit(&mut self, prio: u8, kind: EventKind) -> u64 {
        let t = self.now();
        self.emit_at(t, prio, kind)
    }
}

/// Engine 单次运行的安全预算。
///
/// 默认值提供有限的事件数和因果深度保护，即使 handler 无意中持续自调度，
/// 引擎也会以明确错误终止，而不是无限占用线程。`deadline` 和 `cancel` 只
/// 用作运行安全边界，不参与事件排序，因此不会破坏回放的确定性。
#[derive(Clone, Debug)]
pub struct EngineRunOptions {
    pub max_events: u64,
    pub max_causal_depth: usize,
    pub deadline: Option<Instant>,
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for EngineRunOptions {
    fn default() -> Self {
        Self {
            max_events: 1_000_000,
            max_causal_depth: 1_024,
            deadline: None,
            cancel: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EngineRunReport {
    pub processed_events: u64,
    pub remaining_events: usize,
}

pub trait Handler {
    fn on_event(&mut self, e: &Event, ctx: &mut EngineCtx) -> QxResult<()>;
}

pub struct Engine {
    pub ctx: EngineCtx,
    handlers: Vec<Box<dyn Handler>>,
}

impl Engine {
    pub fn new(start: Ts) -> Self {
        Self {
            ctx: EngineCtx::new(start),
            handlers: Vec::new(),
        }
    }

    pub fn add_handler(&mut self, h: Box<dyn Handler>) {
        self.handlers.push(h);
    }

    /// 运行至队列空。
    pub fn run(&mut self) -> QxResult<()> {
        self.run_with_options(EngineRunOptions::default())
            .map(|_| ())
    }

    /// 按安全预算运行至队列空。
    pub fn run_with_options(&mut self, options: EngineRunOptions) -> QxResult<EngineRunReport> {
        if options.max_events == 0 {
            return Err(QxError::ResourceExhausted(
                "Engine max_events 必须大于 0".into(),
            ));
        }
        let Engine { ctx, handlers } = self;
        let mut processed_events = 0_u64;
        while let Some(e) = ctx.queue.pop() {
            if processed_events >= options.max_events {
                return Err(QxError::ResourceExhausted(format!(
                    "Engine 事件预算耗尽: processed={} max_events={}",
                    processed_events, options.max_events
                )));
            }
            if options
                .cancel
                .as_ref()
                .is_some_and(|cancel| cancel.load(Ordering::Acquire))
            {
                return Err(QxError::ResourceExhausted(
                    "Engine 被取消，剩余事件未处理".into(),
                ));
            }
            if options
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                return Err(QxError::ResourceExhausted(
                    "Engine 达到运行截止时间，剩余事件未处理".into(),
                ));
            }
            let causal_depth = ctx.causal_depths.remove(&e.seq).unwrap_or(0);
            if causal_depth > options.max_causal_depth {
                return Err(QxError::ResourceExhausted(format!(
                    "Engine 因果深度超限: depth={} max_causal_depth={}",
                    causal_depth, options.max_causal_depth
                )));
            }
            // 时间倒流是内部一致性错误，不是可容错情况
            if ctx.clock.advance_to(e.ts).is_err() {
                return Err(QxError::Invariant(format!(
                    "时间倒流: now={} event.ts={}",
                    ctx.clock.now(),
                    e.ts
                )));
            }
            ctx.log.append_checked(e.clone())?;
            ctx.current_event = Some((e.seq, causal_depth));
            for h in handlers.iter_mut() {
                h.on_event(&e, ctx)?;
            }
            ctx.current_event = None;
            processed_events = processed_events.saturating_add(1);
        }
        Ok(EngineRunReport {
            processed_events,
            remaining_events: ctx.queue.len(),
        })
    }

    pub fn result_hash(&self) -> u64 {
        self.ctx.log.digest()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Priority;

    #[test]
    fn engine_drains_in_causal_order() {
        let mut eng = Engine::new(0);
        // 故意乱序投递
        eng.ctx.emit_at(100, Priority::POST, EventKind::Settle);
        eng.ctx.emit_at(50, Priority::MARKET, EventKind::Settle);
        eng.ctx.emit_at(100, Priority::MARKET, EventKind::Settle);

        eng.run().unwrap();

        assert_eq!(eng.ctx.log.len(), 3);
        assert!(eng.ctx.log.validate().is_ok());
        let ts: Vec<u64> = eng.ctx.log.events().iter().map(|e| e.ts).collect();
        assert_eq!(ts, vec![50, 100, 100]);
        let prios: Vec<u8> = eng.ctx.log.events().iter().map(|e| e.prio).collect();
        assert_eq!(
            prios,
            vec![Priority::MARKET, Priority::MARKET, Priority::POST]
        );
    }

    #[test]
    fn handler_can_emit_followup_events() {
        struct Chain;
        impl Handler for Chain {
            fn on_event(&mut self, e: &Event, ctx: &mut EngineCtx) -> QxResult<()> {
                if matches!(e.kind, EventKind::Timer { .. }) {
                    ctx.emit(Priority::COMMAND, EventKind::Settle);
                }
                Ok(())
            }
        }

        let mut eng = Engine::new(0);
        eng.add_handler(Box::new(Chain));
        eng.ctx
            .emit_at(0, Priority::TIMER, EventKind::Timer { name: "t".into() });
        eng.run().unwrap();
        // Timer 处理时投递了后续事件，引擎必须继续处理而非预先展开时间轴
        assert_eq!(eng.ctx.log.len(), 2);
    }

    #[test]
    fn backward_time_is_invariant_error() {
        let mut eng = Engine::new(1000);
        eng.ctx.emit_at(500, Priority::MARKET, EventKind::Settle);
        assert!(matches!(eng.run(), Err(QxError::Invariant(_))));
    }

    #[test]
    fn engine_stops_unbounded_self_scheduling() {
        struct Infinite;
        impl Handler for Infinite {
            fn on_event(&mut self, _e: &Event, ctx: &mut EngineCtx) -> QxResult<()> {
                ctx.emit(Priority::COMMAND, EventKind::Settle);
                Ok(())
            }
        }

        let mut eng = Engine::new(0);
        eng.add_handler(Box::new(Infinite));
        eng.ctx.emit(
            Priority::TIMER,
            EventKind::Timer {
                name: "loop".into(),
            },
        );
        let result = eng.run_with_options(EngineRunOptions {
            max_events: 3,
            ..EngineRunOptions::default()
        });
        assert!(matches!(result, Err(QxError::ResourceExhausted(_))));
        assert_eq!(eng.ctx.log.len(), 3);
    }

    #[test]
    fn engine_enforces_causal_depth_and_cancellation() {
        struct Chain;
        impl Handler for Chain {
            fn on_event(&mut self, e: &Event, ctx: &mut EngineCtx) -> QxResult<()> {
                if matches!(e.kind, EventKind::Timer { .. }) {
                    ctx.emit(Priority::COMMAND, EventKind::Settle);
                }
                Ok(())
            }
        }

        let mut depth_limited = Engine::new(0);
        depth_limited.add_handler(Box::new(Chain));
        depth_limited.ctx.emit(
            Priority::TIMER,
            EventKind::Timer {
                name: "depth".into(),
            },
        );
        let result = depth_limited.run_with_options(EngineRunOptions {
            max_causal_depth: 0,
            ..EngineRunOptions::default()
        });
        assert!(matches!(result, Err(QxError::ResourceExhausted(_))));

        let cancel = Arc::new(AtomicBool::new(true));
        let mut cancelled = Engine::new(0);
        cancelled.ctx.emit(
            Priority::TIMER,
            EventKind::Timer {
                name: "cancel".into(),
            },
        );
        let result = cancelled.run_with_options(EngineRunOptions {
            cancel: Some(cancel),
            ..EngineRunOptions::default()
        });
        assert!(matches!(result, Err(QxError::ResourceExhausted(_))));
        assert!(cancelled.ctx.log.is_empty());
    }
}
