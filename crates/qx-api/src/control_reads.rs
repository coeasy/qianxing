//! 控制面现读：`/control/audit` 与 `/metrics` 的退场计数共用同一次 store 读。
//!
//! worker 进程经 `control_store.transact(execute)` 落盘的命令终态与随之发生的退场不会
//! 回流到 API 进程，所以读启动期副本给的是滞后上界而非实时值（V13 R8）。这里装了 provider
//! 就每次请求现读真实存储，读失败以错误反馈，不回落成启动期副本。

use crate::*;

pub type ControlPlaneProvider = Arc<dyn Fn() -> Result<ControlPlane, String> + Send + Sync>;

impl ApiService {
    /// 控制面真实存储的现读出口：`/control/audit`、`/metrics` 的退场计数与 `QueryPort`
    /// 共用它，避免退回启动期副本，也不让审计与退场摘要各读一份。
    pub fn with_control_plane_provider<F>(mut self, provider: F) -> Self
    where
        F: Fn() -> Result<ControlPlane, String> + Send + Sync + 'static,
    {
        self.control_plane_provider = Some(Arc::new(provider));
        self
    }

    /// 现读控制面并把同一份快照回填进程内副本，令单次请求内的口径不分叉。
    pub(crate) fn control_plane_live(&self) -> Result<ControlPlane, String> {
        let plane = match &self.control_plane_provider {
            Some(provider) => provider()?,
            None => lock_state(&self.state)?.control.clone(),
        };
        lock_state(&self.state)?.control = plane.clone();
        Ok(plane)
    }
}
