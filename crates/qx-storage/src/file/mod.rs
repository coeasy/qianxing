//! 文件后端的可恢复状态存储（V10 §4.9 存储收敛）。
//!
//! 本目录模块收敛四个 JSON 记录存储：[`JsonStateStore`]、[`FileConsumerStateStore`]、
//! [`FileOutboxStore`]、[`FileJobQueue`]。共同形状由 [`JsonRecordStore`] 提供，
//! 序列化/版本/损坏拒绝由 crate 根 `state_envelope` 提供，原子替换只经由唯一
//! helper `write_atomic_path`；本模块的子文件不得再自持 temp+rename 样板。

use super::*;

mod consumers;
mod jobs;
mod outbox;
mod records;
mod state;

pub(crate) use records::{list_json_files, JsonRecordStore};

pub use consumers::FileConsumerStateStore;
pub use jobs::FileJobQueue;
pub use outbox::FileOutboxStore;
pub use state::JsonStateStore;
