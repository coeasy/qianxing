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

pub struct EngineCtx {
    pub clock: TestClock,
    pub queue: CausalQueue,
    pub log: EventLog,
    next_seq: u64,
}

impl EngineCtx {
    pub fn new(start: Ts) -> Self {
        Self {
            clock: TestClock::new(start),
            queue: CausalQueue::new(),
            log: EventLog::new(),
            next_seq: 0,
        }
    }

    pub fn now(&self) -> Ts {
        self.clock.now()
    }

    /// 在指定时刻投递事件，返回 seq。
    pub fn emit_at(&mut self, ts: Ts, prio: u8, kind: EventKind) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.queue.push(Event::new(seq, ts, prio, kind));
        seq
    }

    /// 在当前时刻投递事件。
    pub fn emit(&mut self, prio: u8, kind: EventKind) -> u64 {
        let t = self.now();
        self.emit_at(t, prio, kind)
    }
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
        let Engine { ctx, handlers } = self;
        while let Some(e) = ctx.queue.pop() {
            // 时间倒流是内部一致性错误，不是可容错情况
            if ctx.clock.advance_to(e.ts).is_err() {
                return Err(QxError::Invariant(format!(
                    "时间倒流: now={} event.ts={}",
                    ctx.clock.now(),
                    e.ts
                )));
            }
            ctx.log.append_checked(e.clone())?;
            for h in handlers.iter_mut() {
                h.on_event(&e, ctx)?;
            }
        }
        Ok(())
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
}
