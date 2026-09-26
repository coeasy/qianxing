//! 文件后端的可恢复状态存储（V10 §4.9 存储收敛）。
//!
//! 本目录模块收敛四本 JSON 记录存储：[`JsonStateStore`]、[`FileConsumerStateStore`]、
//! [`FileOutboxStore`]、[`FileJobQueue`]。共同形状由 [`JsonRecordStore`] 提供，
//! 序列化/版本/损坏拒绝由 crate 根 `state_envelope` 提供，原子替换只经由唯一
//! helper `write_atomic_path`；本模块的子文件不得再自持 temp+rename 样板。
//!
//! [`AuditFileStore`] 是第五本，也是唯一不套上面那条规则的：它是追加式哈希审计链，
//! 每次追加重写整段等于把写入频率乘上文件长度，因此自持 JSONL 尾部追加（V11 R5-2）。

use super::*;

mod audit;
mod consumers;
mod jobs;
mod outbox;
mod records;
mod state;

pub(crate) use audit::FileChainWriter;
pub(crate) use records::{list_json_files, JsonRecordStore};

pub use audit::AuditFileStore;
pub use consumers::FileConsumerStateStore;
pub use jobs::FileJobQueue;
pub use outbox::FileOutboxStore;
pub use state::JsonStateStore;
