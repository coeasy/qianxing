//! Durable snapshots for asynchronous HTTP runs.
//!
//! Worker threads cannot survive a process restart. Terminal outcomes are
//! therefore restored verbatim, while a persisted non-terminal run is marked
//! failed with an explicit interruption error instead of appearing live forever.

use super::{AppRunEntry, RunStatus};
use qx_app::{AppError, AppErrorCategory, RunEvent};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const SNAPSHOT_VERSION: u32 = 2;

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RunSnapshot {
    schema_version: u32,
    run_id: String,
    status: String,
    result: Option<String>,
    error: Option<String>,
    #[serde(default)]
    events: Vec<RunEvent>,
}

pub(super) fn load(root: &Path) -> Result<BTreeMap<String, AppRunEntry>, AppError> {
    std::fs::create_dir_all(root).map_err(|error| {
        AppError::from_io(&format!("创建运行注册表目录 {}", root.display()), &error)
    })?;
    let mut entries = BTreeMap::new();
    for item in std::fs::read_dir(root).map_err(|error| {
        AppError::from_io(&format!("读取运行注册表目录 {}", root.display()), &error)
    })? {
        let item = item.map_err(|error| AppError::from_io("读取运行注册表目录项", &error))?;
        let file_type = item
            .file_type()
            .map_err(|error| AppError::from_io("读取运行注册表文件类型", &error))?;
        if !file_type.is_file()
            || item
                .path()
                .extension()
                .is_none_or(|extension| extension != "json")
        {
            continue;
        }
        let path = item.path();
        let body = std::fs::read_to_string(&path).map_err(|error| {
            AppError::from_io(&format!("读取运行记录 {}", path.display()), &error)
        })?;
        let mut snapshot: RunSnapshot = serde_json::from_str(&body).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("运行记录 {} 格式损坏: {error}", path.display()),
            )
        })?;
        if !matches!(snapshot.schema_version, 1 | SNAPSHOT_VERSION)
            || !super::valid_run_route_id(&snapshot.run_id)
            || path.file_stem().and_then(|name| name.to_str()) != Some(snapshot.run_id.as_str())
        {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                format!("运行记录身份或 schema 无效: {}", path.display()),
            ));
        }
        if snapshot
            .events
            .iter()
            .enumerate()
            .any(|(index, event)| event.run_id != snapshot.run_id || event.seq != index as u64 + 1)
        {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                format!("运行记录事件身份或序号无效: {}", path.display()),
            ));
        }
        if !matches!(
            snapshot.status.as_str(),
            "starting" | "running" | "cancelling" | "succeeded" | "cancelled" | "failed"
        ) {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                format!("运行记录状态非法: {}", path.display()),
            ));
        }
        snapshot.schema_version = SNAPSHOT_VERSION;
        if !matches!(
            snapshot.status.as_str(),
            "succeeded" | "cancelled" | "failed"
        ) {
            let run_id = snapshot.run_id.clone();
            snapshot.status = "failed".to_string();
            snapshot.error = Some(
                AppError::new(
                    AppErrorCategory::InternalInvariant,
                    "API 进程在运行期间退出；该任务未恢复执行，请检查产物后再决定是否重跑",
                )
                .with_correlation_id(run_id.clone())
                .to_json(),
            );
            snapshot.events.push(RunEvent {
                seq: snapshot.events.len() as u64 + 1,
                run_id: run_id.clone(),
                kind: qx_app::RunEventKind::Failed,
                status: RunStatus::Failed,
            });
            write_snapshot(root, &snapshot, false)?;
        }
        let run_id = snapshot.run_id.clone();
        entries.insert(
            run_id.clone(),
            AppRunEntry {
                run_id,
                run: None,
                cancel_requested: false,
                watcher_slot_reserved: false,
                result: snapshot.result,
                error: snapshot.error,
                restored_status: Some(snapshot.status),
                events: snapshot.events,
            },
        );
    }
    Ok(entries)
}

pub(super) fn reserve(root: &Path, entry: &mut AppRunEntry) -> Result<(), AppError> {
    let snapshot = snapshot(entry);
    write_snapshot(root, &snapshot, true)
}

pub(super) fn save(root: &Path, entry: &mut AppRunEntry) -> Result<(), AppError> {
    let snapshot = snapshot(entry);
    write_snapshot(root, &snapshot, false)
}

pub(super) fn save_if_changed(
    root: &Path,
    entry: &mut AppRunEntry,
    previous: &mut Option<RunSnapshot>,
) -> Result<(), AppError> {
    let current = snapshot(entry);
    if previous.as_ref() == Some(&current) {
        return Ok(());
    }
    write_snapshot(root, &current, false)?;
    *previous = Some(current);
    Ok(())
}

pub(super) fn remove(root: &Path, run_id: &str) -> Result<(), AppError> {
    match std::fs::remove_file(path_for(root, run_id)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::from_io(
            &format!("清理已过期运行记录 {run_id}"),
            &error,
        )),
    }
}

pub(super) fn reserve_limited_slot(
    counter: &std::sync::atomic::AtomicUsize,
    capacity: usize,
) -> bool {
    use std::sync::atomic::Ordering::{AcqRel, Acquire};

    let mut count = counter.load(Acquire);
    loop {
        if count >= capacity {
            return false;
        }
        match counter.compare_exchange_weak(count, count + 1, AcqRel, Acquire) {
            Ok(_) => return true,
            Err(current) => count = current,
        }
    }
}

pub(super) struct WatcherSlot;

impl Drop for WatcherSlot {
    fn drop(&mut self) {
        super::APP_RUN_WATCHERS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

fn snapshot(entry: &mut AppRunEntry) -> RunSnapshot {
    entry.refresh_events();
    RunSnapshot {
        schema_version: SNAPSHOT_VERSION,
        run_id: entry.run_id.clone(),
        status: if let Some(status) = &entry.restored_status {
            status.clone()
        } else {
            status_name(entry.status()).to_string()
        },
        result: entry.result.clone(),
        error: entry.error.clone(),
        events: entry.events.clone(),
    }
}

fn status_name(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Running => "running",
        RunStatus::Cancelling => "cancelling",
        RunStatus::Succeeded => "succeeded",
        RunStatus::Cancelled => "cancelled",
        RunStatus::Failed => "failed",
    }
}

fn write_snapshot(root: &Path, snapshot: &RunSnapshot, create_only: bool) -> Result<(), AppError> {
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(root).map_err(|error| {
        AppError::from_io(&format!("创建运行注册表目录 {}", root.display()), &error)
    })?;
    let destination = path_for(root, &snapshot.run_id);
    let body = serde_json::to_vec(snapshot).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("序列化运行记录失败: {error}"),
        )
    })?;
    if create_only {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|error| {
                AppError::from_io(&format!("保留运行记录 {}", destination.display()), &error)
            })?;
        file.write_all(&body)
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                AppError::from_io(&format!("持久化运行记录 {}", destination.display()), &error)
            })?;
        return Ok(());
    }
    let serial = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = PathBuf::from(format!(
        "{}.tmp-{}-{serial}",
        destination.display(),
        std::process::id()
    ));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| {
                AppError::from_io(
                    &format!("创建运行记录临时文件 {}", temporary.display()),
                    &error,
                )
            })?;
        file.write_all(&body)
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                AppError::from_io(
                    &format!("写入运行记录临时文件 {}", temporary.display()),
                    &error,
                )
            })?;
        drop(file);
        std::fs::rename(&temporary, &destination).map_err(|error| {
            AppError::from_io(&format!("发布运行记录 {}", destination.display()), &error)
        })
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn path_for(root: &Path, run_id: &str) -> PathBuf {
    root.join(format!("{run_id}.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_surface::AppRunRegistry;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn durable_runs_cannot_overbook_terminal_persistence_watchers() {
        let slots = std::sync::atomic::AtomicUsize::new(0);
        assert!(reserve_limited_slot(&slots, 2));
        assert!(reserve_limited_slot(&slots, 2));
        assert!(!reserve_limited_slot(&slots, 2));
        assert_eq!(slots.load(std::sync::atomic::Ordering::Acquire), 2);
    }

    fn root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "qx-api-run-registry-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn terminal_result_survives_registry_reopen() {
        let root = root("terminal");
        let registry = AppRunRegistry::open(root.clone());
        registry.reserve("persisted-success".to_string()).unwrap();
        {
            let mut entries = registry.entries.lock().unwrap();
            let entry = entries.get_mut("persisted-success").unwrap();
            entry.restored_status = Some("succeeded".to_string());
            entry.result = Some(r#"{"value":42}"#.to_string());
            entry.events = vec![
                RunEvent {
                    seq: 1,
                    run_id: "persisted-success".into(),
                    kind: qx_app::RunEventKind::Started,
                    status: RunStatus::Running,
                },
                RunEvent {
                    seq: 2,
                    run_id: "persisted-success".into(),
                    kind: qx_app::RunEventKind::Succeeded,
                    status: RunStatus::Succeeded,
                },
            ];
            save(&root, entry).unwrap();
        }
        drop(registry);

        let reopened = AppRunRegistry::open(root.clone());
        let response = reopened.status("persisted-success");
        let document: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(document["status"], "succeeded");
        assert_eq!(document["result"]["value"], 42);
        assert!(document["error"].is_null());
        let events = reopened.events("persisted-success", 1);
        let events: serde_json::Value = serde_json::from_str(&events.body).unwrap();
        assert_eq!(events[0]["seq"], 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_run_left_active_by_process_exit_recovers_as_failed_not_running() {
        let root = root("interrupted");
        let registry = AppRunRegistry::open(root.clone());
        registry.reserve("persisted-active".to_string()).unwrap();
        drop(registry);

        let reopened = AppRunRegistry::open(root.clone());
        let response = reopened.status("persisted-active");
        let document: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(document["status"], "failed");
        assert_eq!(document["error"]["category"], "INTERNAL_INVARIANT");
        assert!(document["error"]["message"]
            .as_str()
            .unwrap()
            .contains("进程在运行期间退出"));
        let events = reopened.events("persisted-active", 0);
        let events: serde_json::Value = serde_json::from_str(&events.body).unwrap();
        assert_eq!(events[0]["kind"], "failed");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_persisted_run_fails_registry_closed() {
        let root = root("corrupt");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("broken.json"), b"not-json").unwrap();
        let registry = AppRunRegistry::open(root.clone());
        let response = registry.status("broken");
        let document: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(response.status, 503);
        assert_eq!(document["category"], "STORAGE_FAILURE");
        std::fs::remove_dir_all(root).unwrap();
    }
}
