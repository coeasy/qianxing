//! EventLog 后端切换闸门。
//!
//! 单文件后端写 `{name}.json`，分段后端写 `{name}.manifest.json` + `segments/`，两套文件后端
//! 在同一 `storage.root` 下互不相交，而打开时「自己那份文件不在」一律当作首次启动。于是改动
//! `storage.event_log_segment_events`（或从文件后端换成 SQLite / PostgreSQL）会让运行时读到
//! 0 条事实、账簿归零，并从 seq 0 在同一目录里写出**第二本**账。这里把那条静默换账的通路
//! 关成 fail-closed：发现另一本历史就当场拒绝启动。

use qx_core::QxError;
use qx_storage::{EventLogFileStore, SegmentedEventLogStore, StorageError};

use super::{is_valid_log_name, RuntimeEventStore};

/// 换后端时留在原地的另一本历史：后端名、承载它的文件，以及它已有多少条事实。
struct AbandonedBackendLog {
    label: &'static str,
    path: std::path::PathBuf,
    events: usize,
}

/// 当前选中的后端在报错文案里的名字。
fn backend_label(store: &RuntimeEventStore) -> &'static str {
    match store {
        RuntimeEventStore::Flat(..) => "单文件",
        RuntimeEventStore::Segmented(..) => "分段",
        #[cfg(feature = "sqlite")]
        RuntimeEventStore::Sqlite(..) => "SQLite",
        #[cfg(feature = "postgres")]
        RuntimeEventStore::Postgres(..) => "PostgreSQL",
    }
}

/// 同一目录下，**当前配置没选中**的那套文件后端是否已经存着这个账户的事实。
/// 数据库后端的数据不在 `root` 里，由 [`guard_database_backend`] 单独查文件一侧。
fn abandoned_log_for(
    store: &RuntimeEventStore,
    name: &str,
) -> Result<Option<AbandonedBackendLog>, StorageError> {
    match store {
        RuntimeEventStore::Flat(event_store, _) => {
            abandoned_file_log(event_store.root(), name, false)
        }
        RuntimeEventStore::Segmented(event_store, _) => {
            abandoned_file_log(event_store.root(), name, true)
        }
        #[cfg(feature = "sqlite")]
        RuntimeEventStore::Sqlite(_) => Ok(None),
        #[cfg(feature = "postgres")]
        RuntimeEventStore::Postgres(_) => Ok(None),
    }
}

/// 文件后端之间换选的闸门：`LiveEventPipeline` 的三条打开入口共用。
pub(super) fn guard_file_backends(store: &RuntimeEventStore, name: &str) -> Result<(), QxError> {
    let abandoned = abandoned_log_for(store, name).map_err(super::storage_error)?;
    match abandoned {
        Some(abandoned) => Err(switch_error(
            name,
            backend_label(store),
            std::slice::from_ref(&abandoned),
        )),
        None => Ok(()),
    }
}

/// 选定**数据库**后端之前要过的闸门：`storage.root` 下还留着这个账户的文件后端历史就当场拒绝。
///
/// 数据库后端读不到文件，换过去之后旧历史会变成没人再读的文件，而数据库里的空表被当作首次启动
/// —— 账户快照当场归零，且旧文件随后被当成一次性产物清掉。这里只查文件一侧，反向（数据库→文件）
/// 由数据库后端的空表判断不了，需要迁移时人工核对。非法 `log_name` 不在这里报错，留给真正的
/// 打开入口，免得同一名义两处口径。
pub(super) fn guard_database_backend(root: &std::path::Path, name: &str) -> Result<(), QxError> {
    if !is_valid_log_name(name) {
        return Ok(());
    }
    let abandoned = abandoned_file_logs(root, name).map_err(super::storage_error)?;
    if abandoned.is_empty() {
        return Ok(());
    }
    Err(switch_error(name, "数据库", &abandoned))
}

/// 当前配置没选中的那套文件后端里的历史。
fn abandoned_file_log(
    root: &std::path::Path,
    name: &str,
    chosen_segmented: bool,
) -> Result<Option<AbandonedBackendLog>, StorageError> {
    let (label, log) = if chosen_segmented {
        ("单文件", EventLogFileStore::new(root).read_if_exists(name)?)
    } else {
        // 分段大小只决定写入怎么切段，读回完全按 manifest 走；这里给 1 只是为了拿到
        // 一个只读句柄，不影响读到的事实。
        (
            "分段",
            SegmentedEventLogStore::new(root, 1)?.read_if_exists(name)?,
        )
    };
    let Some(log) = log.filter(|log| !log.is_empty()) else {
        return Ok(None);
    };
    Ok(Some(AbandonedBackendLog {
        label,
        path: root.join(if label == "分段" {
            format!("{name}.manifest.json")
        } else {
            format!("{name}.json")
        }),
        events: log.len(),
    }))
}

/// 两套文件后端里这个账户的历史：数据库后端一份都用不上。
fn abandoned_file_logs(
    root: &std::path::Path,
    name: &str,
) -> Result<Vec<AbandonedBackendLog>, StorageError> {
    let mut found = Vec::new();
    for chosen_segmented in [false, true] {
        if let Some(abandoned) = abandoned_file_log(root, name, chosen_segmented)? {
            found.push(abandoned);
        }
    }
    Ok(found)
}

/// 换后端的报错。这不是重试能解决的问题：要么改回原后端，要么把旧历史归档到别处，
/// 两条出路都写进文案，别让运维靠猜。
fn switch_error(log_name: &str, chosen_label: &str, abandoned: &[AbandonedBackendLog]) -> QxError {
    let detail = abandoned
        .iter()
        .map(|item| {
            format!(
                "{}后端 {path}（{events} 条事实）",
                item.label,
                path = item.path.display(),
                events = item.events
            )
        })
        .collect::<Vec<_>>()
        .join("、");
    QxError::Permanent(format!(
        "EventLog「{log_name}」选中的是{chosen_label}后端，但同一目录下还留着另一本历史：{detail}。\
         换后端不会自动迁移历史，直接启动会让账户读到空账本、账簿归零，并从 seq 0 开始写第二本账。\
         请改回原来读取的那套后端，或把上面这些文件（分段后端另有 segments/ 目录）归档到别的数据目录后再启动。"
    ))
}
