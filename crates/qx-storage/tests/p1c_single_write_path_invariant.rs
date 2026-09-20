//! P1c 架构不变量（V10 §4.9）：一套信封写路径、一套退避/尝试计数口径。
//!
//! 本用例是源码级反向验证锚点：任何把逻辑重新就地复制回调用方的改动都必须让
//! 它变红（与 `tools/check_architecture.py` 的登记口径配套）。
//! - 指数增长的算术只允许存在于 `crates/qx-core/src/retry.rs`；
//! - 存储四个文件状态存储（`JsonStateStore` / `FileConsumerStateStore` /
//!   `FileOutboxStore` / `FileJobQueue`）的读写代码只能出现在
//!   `crates/qx-storage/src/state_envelope.rs`（信封层），其存储主体收敛在
//!   `crates/qx-storage/src/file/` 目录模块，二者都不得自带就地序列化 / 原子替换；
//! - 尝试计数（attempts）不得在各后端就地推导算术，必须由
//!   `RetryPolicy::next_attempt_count` 单一口径驱动。

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-storage 必须位于 <workspace>/crates/ 下")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = workspace_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("读取 {} 失败: {error}", path.display()))
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("目录必须可读") {
        let path = entry.expect("目录项").path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn exponential_backoff_arithmetic_exists_once() {
    let root = workspace_root();
    let crates = root.join("crates");
    let mut files = Vec::new();
    // 只扫描生产源码 `crates/*/src`（与行数棘轮 / check_architecture 口径一致），
    // 避免本用例自身或其他测试字符串触发误报。
    for crate_dir in std::fs::read_dir(&crates).expect("crates 目录必须可读") {
        let src = crate_dir.expect("crate 目录项").path().join("src");
        if src.is_dir() {
            collect_rs(&src, &mut files);
        }
    }
    let mut offenders = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        let normalized: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
        if (normalized.contains("1_u128<<exponent") || normalized.contains("max_delay.as_millis()"))
            && path != root.join("crates/qx-core/src/retry.rs")
        {
            offenders.push(path);
        }
    }
    assert!(
        offenders.is_empty(),
        "指数退避算术必须在 qx-core/src/retry.rs 单点实现，发现重复: {offenders:?}"
    );
    // 反向锚点：统一实现必须仍然真实存在（防止删光算术让扫描空转）。
    let core = read("crates/qx-core/src/retry.rs");
    let normalized: String = core.chars().filter(|ch| !ch.is_whitespace()).collect();
    assert!(normalized.contains("1_u128<<exponent"));

    // 调用方必须确实委托统一策略，而不是保留平行实现。
    assert!(read("crates/qx-adapter/src/binance.rs").contains("retry::RetryPolicy"));
    assert!(read("crates/qx-scheduler/src/retry_policy.rs").contains("qx_core::retry"));
}

#[test]
fn four_file_state_stores_share_one_envelope_io() {
    let lib = read("crates/qx-storage/src/lib.rs");
    let envelope = read("crates/qx-storage/src/state_envelope.rs");
    // 原子替换 / 锁 / fsync 的唯一实现只住进信封层。
    assert!(!envelope.is_empty());
    assert!(envelope.contains("fn write_atomic_path"));
    assert!(envelope.contains("fn acquire_storage_lock"));
    assert!(envelope.contains("fn sync_file"));
    // lib.rs 迁移后仍不得自带原子替换 / 锁 / fsync 实现。
    assert!(!lib.contains("fn write_atomic_path"));
    assert!(!lib.contains("fn acquire_storage_lock"));
    assert!(!lib.contains("fn sync_file"));

    // 四个存储已从 lib.rs 迁入 `crates/qx-storage/src/file/` 目录模块；该目录内
    // 同样不允许出现任何“就地序列化 / 就地读写状态文件 / 自带原子替换”代码。
    let store_dir = workspace_root().join("crates/qx-storage/src/file");
    let mut store_files = Vec::new();
    collect_rs(&store_dir, &mut store_files);
    assert!(
        store_files.len() >= 5,
        "file 目录模块必须包含四个存储 + records 共享形状，实际 {store_files:?}"
    );
    for path in &store_files {
        let text = std::fs::read_to_string(path).unwrap();
        let normalized: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
        for forbidden in [
            "fnwrite_atomic_path",
            "fnacquire_storage_lock",
            "fnsync_file",
            "std::fs::read_to_string(",
            "serde_json::to_string",
            "serde_json::from_str",
        ] {
            assert!(
                !normalized.contains(forbidden),
                "文件存储 {} 必须经由 state_envelope 读写与原子替换，发现就地实现 {forbidden:?}",
                path.display()
            );
        }
    }

    // 正向锚点：四个存储确实调用信封原语（防止把调用删空来绕过扫描）。
    let consumer = read("crates/qx-storage/src/file/consumers.rs");
    assert!(consumer.contains("transact_state_json") || consumer.contains("read_state_json"));
    let outbox = read("crates/qx-storage/src/file/outbox.rs");
    assert!(outbox.contains("read_state_json"));
    assert!(outbox.contains("write_state_json"));
    let queue = read("crates/qx-storage/src/file/jobs.rs");
    assert!(queue.contains("read_state_json"));
    assert!(queue.contains("write_state_json"));
    let json_state = read("crates/qx-storage/src/file/state.rs");
    assert!(json_state.contains("read_json_file"));
    assert!(json_state.contains("write_json_file"));
    assert!(json_state.contains("write_state_text"));
}

#[test]
fn attempt_counting_uses_the_single_policy_arithmetic() {
    let lib = read("crates/qx-storage/src/lib.rs");
    let outbox = read("crates/qx-storage/src/file/outbox.rs");
    let sqlite = read("crates/qx-storage/src/sqlite.rs");
    let postgres = read("crates/qx-storage/src/postgres.rs");
    for (name, source) in [
        ("lib.rs", &lib),
        ("file/outbox.rs", &outbox),
        ("sqlite.rs", &sqlite),
        ("postgres.rs", &postgres),
    ] {
        let normalized: String = source.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert!(
            !normalized.contains("attempts.saturating_add(1)")
                && !normalized.contains("attemptsASINTEGER)+1")
                && !(normalized.contains("attempts::numeric+1)::text")
                    && !normalized.contains("LEAST(attempts::numeric+1")),
            "{name} 的尝试计数必须收敛到统一口径，不得就地推导 +1 算术"
        );
    }
    // 文件后端的尝试计数已从 lib.rs 迁入 file/outbox.rs，口径仍来自唯一策略。
    assert!(outbox.contains("RetryPolicy::next_attempt_count"));
    assert!(sqlite.contains("RetryPolicy::next_attempt_count"));
    assert!(postgres.contains("LEAST(attempts::numeric + 1"));
}
