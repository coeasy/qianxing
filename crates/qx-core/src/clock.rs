//! 时间戳类型：内核与回测共用的时间单位口径。
//!
//! 这里**不提供虚拟时钟**。回测的时间轴完全由输入数据自带的时间戳决定
//! （`Bar.timestamp` / `BarFrame.ts` / `Event::ts`），推进顺序由数据侧闸门守住：
//! `qx-data::process_bars` 先按 `(instrument, timestamp)` 排序，再交 `validate_bars`
//! 拒绝同一标的非严格递增的时间戳；帧读侧 `JsonBarFrameProvider` 对乱序同样直接报错。
//! 因此内核不需要一个"只在显式推进时前进"的时钟对象 —— 那种对象在本仓库里从未被任何
//! 一条链推进过，留着只会让文档承诺一件没发生的事。真实墙钟只出现在 paper/live 的
//! worker 循环里，回测与重放路径不读它。

/// 纳秒时间戳。
pub type Ts = u64;
