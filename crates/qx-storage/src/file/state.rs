//! 通用 JSON 状态存储（控制面 / 调度器 / 任意受约束的 JSON 快照）。
//!
//! P1c（§4.9）收敛：所有落盘写一律经由 `state_envelope` 的原子替换 helper
//! （`write_json_file` / `write_state_text` / `read_json_file` / `read_state_text*`）；
//! 本模块不再自持 temp+rename 样板，且保持对 qx-cli 的公开方法签名逐字不变。

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

    pub fn load_json_at<T: DeserializeOwned>(
        &self,
        relative_path: impl AsRef<Path>,
    ) -> Result<T, StorageError> {
        let path = self.state_path(relative_path)?;
        read_json_file(&path, "JSON 状态")
    }

    pub fn save_control(&self, plane: &ControlPlane) -> Result<PathBuf, StorageError> {
        std::fs::create_dir_all(&self.root).map_err(|error| StorageError::Io(error.to_string()))?;
        let _lock = acquire_storage_lock(self.root.join(".control-plane.write.lock"))?;
        self.save_control_unlocked(plane)
    }

    /// 在同一把文件锁内读取、修改并原子保存控制面状态。
    ///
    /// API 接收命令和执行器回写终态都必须通过这个事务边界，避免两个进程
    /// 分别基于旧快照保存而互相覆盖命令或审计尾部。
    pub fn update_control<T, F>(&self, update: F) -> Result<(ControlPlane, T), StorageError>
    where
        F: FnOnce(&mut ControlPlane) -> Result<T, String>,
    {
        let (plane, result) = self.transact_control(update)?;
        result.map(|value| (plane, value)).map_err(StorageError::Io)
    }

    /// 控制面事务的保留错误类型版本。业务拒绝（重复请求/权限不足等）不会
    /// 被误包装成存储故障，也不会写入半成品状态；只有成功变更才会原子保存。
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
        }
        Ok((plane, result))
    }

    pub fn load_control(&self) -> Result<ControlPlane, StorageError> {
        let text = read_state_text_required(&self.root.join("control-plane.json"))?;
        ControlPlane::from_json(&text).map_err(StorageError::Io)
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

    pub fn save_scheduler(&self, scheduler: &Scheduler) -> Result<PathBuf, StorageError> {
        self.save_scheduler_at("scheduler.json", scheduler)
    }

    pub fn load_scheduler(&self) -> Result<Scheduler, StorageError> {
        self.load_scheduler_at("scheduler.json")
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
