//! 通用 JSON 状态存储（控制面 / 调度器 / 任意受约束的 JSON 快照）。
//!
//! P1c（§4.9）收敛：所有落盘写一律经由 `state_envelope` 的原子替换 helper
//! （`write_json_file` / `write_state_text` / `read_state_text*`）；
//! 本模块不再自持 temp+rename 样板。
//!
//! 这里只保留有生产读者或对外契约读者的入口：写侧 `save_json_at`（对账
//! worker 落报告）、`save_scheduler_at` / `transact_*`，读侧
//! `load_control_if_exists`（runtime 重启装载）/ `load_scheduler_at`
//! （API 现读调度）。
//!
//! 已删除的公共面都是"同一件事的第二答案"，且零读者：
//! - `save_control` / `load_control` / `update_control`：非事务的整体覆盖与
//!   `transact_control` 会在两个进程间互相丢更新与审计尾部；
//! - `save_scheduler` / `load_scheduler`：隐式的默认 `scheduler.json` 与多运行
//!   拓扑（每个运行有自己的 `state_path`）冲突。
//!
//! `load_json_at` 是唯一按受约束路径读回 `reconcile/<worker-id>.json` 的入口
//! （deploy/README.md 把该报告文件写成对外运维契约），与 `save_json_at` 共用
//! 同一套路径越界防线；API 侧的目录扫描读的是全部 worker 的报告，两者不重复。

use super::*;

#[derive(Clone, Debug)]
pub struct JsonStateStore {
    root: PathBuf,
}

impl JsonStateStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 保存一个受路径约束、原子替换的 JSON 状态文件。
    ///
    /// 运行时报告、对账结果等非 Kernel 事实可以复用这个边界；调用方仍应
    /// 把真正的交易事实写入 EventLog，不能用状态文件替代事件追加。
    pub fn save_json_at<T: Serialize>(
        &self,
        relative_path: impl AsRef<Path>,
        value: &T,
    ) -> Result<PathBuf, StorageError> {
        let path = self.state_path(relative_path)?;
        std::fs::create_dir_all(&self.root).map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.root.join(".json-state.write.lock"))?;
        write_json_file(&self.root, &path, value, true, "JSON 状态")?;
        Ok(path)
    }

    /// 按 `save_json_at` 的同一套路径约束读回状态文件。
    ///
    /// 这是写侧唯一带越界防线的对称读法：绕过它直接读盘会丢掉
    /// "相对路径不得越出 data root" 的检查。
    pub fn load_json_at<T: DeserializeOwned>(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<T, StorageError> {
        let path = self.state_path(relative_path)?;
        read_json_file(&path, "JSON 状态")
    }

    /// 在同一把文件锁内读取、修改并原子保存控制面状态。
    ///
    /// API 接收命令和执行器回写终态都必须通过这个事务边界，避免两个进程
    /// 分别基于旧快照保存而互相覆盖命令或审计尾部。业务拒绝（重复请求/权限
    /// 不足等）不会被误包装成存储故障，也不会写入半成品状态；只有成功变更
    /// 才会原子保存，并把这一步新增的审计尾部追加进同 root 的哈希链。
    pub fn transact_control<T, E, F>(
        &self,
        update: F,
    ) -> Result<(ControlPlane, Result<T, E>), StorageError>
    where
        F: FnOnce(&mut ControlPlane) -> Result<T, E>,
    {
        std::fs::create_dir_all(&self.root).map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.root.join(".control-plane.write.lock"))?;
        let path = self.root.join("control-plane.json");
        let mut plane = match read_state_text(&path)? {
            Some(content) => ControlPlane::from_json(&content).map_err(StorageError::Io)?,
            None => ControlPlane::default(),
        };
        let result = update(&mut plane);
        if result.is_ok() {
            self.save_control_unlocked(&plane)?;
            // 控制面每次成功提交都把审计尾部补进只追加的哈希链。顺序不能反：
            // 链落后于快照会被下一次事务自愈，链超前于快照则让 sync_control 的
            // 前缀校验从此永久失败（快照回滚不了已经多出来的链尾）。
            AuditFileStore::new(&self.root).sync_control(&plane)?;
        }
        Ok((plane, result))
    }

    /// 加载可选的控制面状态；首次启动没有文件时返回空控制面。
    pub fn load_control_if_exists(&self) -> Result<Option<ControlPlane>, StorageError> {
        match read_state_text(&self.root.join("control-plane.json"))? {
            Some(text) => Ok(Some(
                ControlPlane::from_json(&text).map_err(StorageError::Io)?,
            )),
            None => Ok(None),
        }
    }

    /// 将调度状态保存到 data root 下的显式相对路径；路径越界会被拒绝。
    /// 这让多个运行拓扑可以在同一个 data root 中拥有独立的调度状态文件。
    pub fn save_scheduler_at(
        &self,
        relative_path: impl AsRef<Path>,
        scheduler: &Scheduler,
    ) -> Result<PathBuf, StorageError> {
        let path = self.state_path(relative_path)?;
        std::fs::create_dir_all(&self.root).map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.root.join(".scheduler.write.lock"))?;
        let content = scheduler.to_json().map_err(StorageError::Io)?;
        write_state_text(&self.root, &path, &content)?;
        Ok(path)
    }

    pub fn load_scheduler_at(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<Scheduler, StorageError> {
        let path = self.state_path(relative_path)?;
        let text = read_state_text_required(&path)?;
        Scheduler::from_json(&text).map_err(StorageError::Io)
    }

    /// 在调度状态文件锁内读取、修改并原子保存；用于 Scheduler 与 Strategy
    /// 进程对同一 JobRun 的异步状态推进，避免旧内存快照覆盖新终态。
    pub fn transact_scheduler_at<T, E, F>(
        &self,
        relative_path: impl AsRef<Path>,
        update: F,
    ) -> Result<(Scheduler, Result<T, E>), StorageError>
    where
        F: FnOnce(&mut Scheduler) -> Result<T, E>,
    {
        let path = self.state_path(relative_path)?;
        std::fs::create_dir_all(&self.root).map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.root.join(".scheduler.write.lock"))?;
        let mut scheduler = match read_state_text(&path)? {
            Some(content) => Scheduler::from_json(&content).map_err(StorageError::Io)?,
            None => Scheduler::default(),
        };
        let result = update(&mut scheduler);
        if result.is_ok() {
            let content = scheduler.to_json().map_err(StorageError::Io)?;
            write_state_text(&self.root, &path, &content)?;
        }
        Ok((scheduler, result))
    }

    fn state_path(&self, relative_path: impl AsRef<Path>) -> Result<PathBuf, StorageError> {
        let relative_path = relative_path.as_ref();
        if relative_path.as_os_str().is_empty() {
            return Err(StorageError::InvalidName("调度状态路径必须非空".into()));
        }
        if relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(StorageError::InvalidName(
                "调度状态路径不能包含父目录".into(),
            ));
        }
        let path = if relative_path.is_absolute() {
            relative_path.to_path_buf()
        } else {
            self.root.join(relative_path)
        };
        if !path.starts_with(&self.root) {
            return Err(StorageError::InvalidName(
                "调度状态路径越出 data root".into(),
            ));
        }
        Ok(path)
    }

    fn save_control_unlocked(&self, plane: &ControlPlane) -> Result<PathBuf, StorageError> {
        let path = self.root.join("control-plane.json");
        write_state_text(
            &self.root,
            &path,
            &plane.to_json().map_err(StorageError::Io)?,
        )?;
        Ok(path)
    }
}
