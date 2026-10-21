//! Compatibility import for the CLI's existing worker entry modules.

#[allow(unused_imports)] // 默认特性下仅由启用 NATS 的 event pipeline 调用
pub(crate) use qx_runtime::host::join_worker_handle;
