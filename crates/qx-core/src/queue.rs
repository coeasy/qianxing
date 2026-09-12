//! 因果事件队列：按 (ts, prio, seq) 全序出队。
//!
//! 全序是确定性的前提：只要输入事件集合相同，出队顺序必然相同，
//! 与插入顺序、哈希迭代顺序、线程调度全部无关。

use crate::event::Event;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

struct Keyed(Event);

impl PartialEq for Keyed {
    fn eq(&self, other: &Self) -> bool {
        self.0.seq == other.0.seq
    }
}
impl Eq for Keyed {}

impl Ord for Keyed {
    /// 反转比较：让 (ts, prio, seq) 最小者在最大堆中排最前。
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .0
            .ts
            .cmp(&self.0.ts)
            .then_with(|| other.0.prio.cmp(&self.0.prio))
            .then_with(|| other.0.seq.cmp(&self.0.seq))
    }
}

impl PartialOrd for Keyed {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Default)]
pub struct CausalQueue {
    heap: BinaryHeap<Keyed>,
}

impl CausalQueue {
    pub fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
        }
    }

    pub fn push(&mut self, e: Event) {
        self.heap.push(Keyed(e));
    }

    pub fn pop(&mut self) -> Option<Event> {
        self.heap.pop().map(|k| k.0)
    }

    pub fn len(&self) -> usize {
        self.heap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Ts;
    use crate::event::{Event, EventKind, Priority};

    fn ev(seq: u64, ts: Ts, prio: u8) -> Event {
        Event::new(seq, ts, prio, EventKind::Settle)
    }

    #[test]
    fn orders_by_ts_then_prio_then_seq() {
        let mut q = CausalQueue::new();
        // 故意乱序插入
        q.push(ev(3, 100, Priority::POST));
        q.push(ev(1, 100, Priority::MARKET));
        q.push(ev(2, 50, Priority::COMMAND));
        q.push(ev(4, 100, Priority::MARKET));

        assert_eq!(q.pop().unwrap().seq, 2); // ts=50 最早
        assert_eq!(q.pop().unwrap().seq, 1); // ts=100, MARKET, seq 1
        assert_eq!(q.pop().unwrap().seq, 4); // ts=100, MARKET, seq 4
        assert_eq!(q.pop().unwrap().seq, 3); // ts=100, POST
        assert!(q.pop().is_none());
    }

    #[test]
    fn same_timestamp_market_before_command() {
        // 这条断言就是"禁止先交易后收行情"的机器化表达
        let mut q = CausalQueue::new();
        q.push(ev(1, 100, Priority::COMMAND));
        q.push(ev(2, 100, Priority::MARKET));
        assert_eq!(q.pop().unwrap().prio, Priority::MARKET);
    }
}
