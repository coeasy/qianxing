use super::*;

impl AppRunEntry {
    pub(super) fn status(&mut self) -> RunStatus {
        match &mut self.run {
            None => RunStatus::Running,
            Some(AppRun::Bar(handle)) => handle.status(),
            Some(AppRun::Depth(handle)) => handle.status(),
            Some(AppRun::DepthExperiment(handle)) => handle.status(),
            Some(AppRun::Experiment(handle)) => handle.status(),
            Some(AppRun::MultiLeg(handle)) => handle.status(),
            Some(AppRun::MultiLegExperiment(handle)) => handle.status(),
            Some(AppRun::MultiAccount(handle)) => handle.status(),
            Some(AppRun::MultiMarket(handle)) => handle.status(),
        }
    }

    pub(super) fn cancel(&mut self) {
        if self.restored_status.is_some() {
            return;
        }
        self.cancel_requested = true;
        match &mut self.run {
            Some(AppRun::Bar(handle)) => handle.cancel(),
            Some(AppRun::Depth(handle)) => handle.cancel(),
            Some(AppRun::DepthExperiment(handle)) => handle.cancel(),
            Some(AppRun::Experiment(handle)) => handle.cancel(),
            Some(AppRun::MultiLeg(handle)) => handle.cancel(),
            Some(AppRun::MultiLegExperiment(handle)) => handle.cancel(),
            Some(AppRun::MultiAccount(handle)) => handle.cancel(),
            Some(AppRun::MultiMarket(handle)) => handle.cancel(),
            None => {}
        }
    }

    pub(super) fn collect_result(&mut self) {
        self.refresh_events();
        let status = self.status();
        if !is_terminal(status) || self.result.is_some() || self.error.is_some() {
            return;
        }
        let outcome = match &mut self.run {
            None => return,
            Some(AppRun::Bar(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::Depth(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::DepthExperiment(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::Experiment(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::MultiLeg(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::MultiLegExperiment(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::MultiAccount(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
            Some(AppRun::MultiMarket(handle)) => handle
                .try_take_result()
                .map(|result| result.and_then(|value| value.to_json())),
        };
        if let Some(outcome) = outcome {
            match outcome {
                Ok(document) => self.result = Some(document),
                Err(error) => self.error = Some(error.to_json()),
            }
        }
    }

    pub(super) fn document(&mut self) -> String {
        self.refresh_events();
        if let Some(status) = &self.restored_status {
            return serde_json::json!({
                "run_id": self.run_id,
                "status": status,
                "result": self.result.as_ref().and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok()),
                "error": self.error.as_ref().and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok()),
            }).to_string();
        }
        self.collect_result();
        let status = match self.status() {
            RunStatus::Running => "running",
            RunStatus::Cancelling => "cancelling",
            RunStatus::Succeeded => "succeeded",
            RunStatus::Cancelled => "cancelled",
            RunStatus::Failed => "failed",
        };
        serde_json::json!({
            "run_id": self.run_id,
            "status": if self.run.is_none() { "starting" } else { status },
            "result": self.result.as_ref().and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok()),
            "error": self.error.as_ref().and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok()),
        })
        .to_string()
    }

    pub(super) fn refresh_events(&mut self) {
        let Some(run) = &self.run else { return };
        let latest = match run {
            AppRun::Bar(handle) => handle.events_after(0),
            AppRun::Depth(handle) => handle.events_after(0),
            AppRun::DepthExperiment(handle) => handle.events_after(0),
            AppRun::Experiment(handle) => handle.events_after(0),
            AppRun::MultiLeg(handle) => handle.events_after(0),
            AppRun::MultiLegExperiment(handle) => handle.events_after(0),
            AppRun::MultiAccount(handle) => handle.events_after(0),
            AppRun::MultiMarket(handle) => handle.events_after(0),
        };
        if latest.len() > self.events.len() {
            self.events = latest;
        }
    }

    pub(super) fn events_document(&mut self, cursor: u64) -> Result<String, AppError> {
        self.refresh_events();
        events::document(&self.events, cursor)
    }
}

pub(crate) fn is_terminal(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Succeeded | RunStatus::Cancelled | RunStatus::Failed
    )
}
pub(crate) fn status_route_id(route: &str) -> Option<&str> {
    let prefix = RUN_STATUS_ROUTE_TEMPLATE.strip_suffix("{run_id}")?;
    route
        .strip_prefix(prefix)
        .filter(|id| valid_run_route_id(id))
}

pub(crate) fn cancel_route_id(route: &str) -> Option<&str> {
    let (prefix, suffix) = RUN_CANCEL_ROUTE_TEMPLATE.split_once("{run_id}")?;
    route
        .strip_prefix(prefix)
        .and_then(|run_id| run_id.strip_suffix(suffix))
        .filter(|id| valid_run_route_id(id))
}

pub(crate) fn valid_run_route_id(run_id: &str) -> bool {
    !run_id.is_empty()
        && !run_id.contains('/')
        && run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}
