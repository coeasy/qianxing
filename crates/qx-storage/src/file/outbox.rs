//! 文件 Outbox 存储（V10 §4.9 收敛进 `file` 目录模块）。
//!
//! 序列化 / 解析 / 版本与损坏拒绝走统一信封（`state_envelope`），原子替换走
//! [`JsonRecordStore`] 的共享写路径；目录枚举复用 `list_records`。

use super::*;

#[derive(Clone, Debug)]
pub struct FileOutboxStore {
    root: PathBuf,
}

impl JsonRecordStore for FileOutboxStore {
    fn records_dir(&self) -> PathBuf {
        self.root.join("outbox/events")
    }
}

impl FileOutboxStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn append(&self, event: OutboxEvent) -> Result<(), StorageError> {
        event.validate()?;
        self.ensure_dirs()?;
        let path = self.event_path(&event.event_id)?;
        let _lock = acquire_storage_lock(self.root.join("outbox.append.lock"))?;
        if path.exists() {
            let existing: OutboxEvent = read_state_json(&path)?;
            if existing.same_fact(&event) {
                return Ok(());
            }
            return Err(StorageError::Conflict(format!(
                "event_id {} 已被不同 Outbox 事件占用",
                event.event_id
            )));
        }
        write_state_json(&self.root, &path, &event)
    }

    pub fn available(&self, now: u64, limit: usize) -> Result<Vec<OutboxEvent>, StorageError> {
        self.ensure_dirs()?;
        // 两组各自按三元组数字序，用尽预算的那组排在后面再截页：它们仍留在候选集里等人工
        // claim/ack（K2 的出口），只是不再占住页首把投得出去的事件挤出一页（V11 R7-d）。
        let mut deliverable = Vec::new();
        let mut parked = Vec::new();
        for path in self.list_records()? {
            let event: OutboxEvent = read_state_json(&path)?;
            let lease_path = self.lease_path(&event.event_id)?;
            if lease_path.exists() && read_state_json::<OutboxLease>(&lease_path)?.expires_ts > now
            {
                continue;
            }
            if crate::outbox_exhausted(event.attempts) {
                parked.push(event);
            } else {
                deliverable.push(event);
            }
        }
        for group in [&mut deliverable, &mut parked] {
            group.sort_by_key(|event| (event.created_ts, event.sequence, event.event_id.clone()));
        }
        deliverable.extend(parked);
        deliverable.truncate(limit);
        Ok(deliverable)
    }

    /// 不问 `limit` 的停摆条数：`available` 截页后逐行数只是这一页的观察值（V11 R7-d）。
    /// 文件后端没有索引，这一问与 `available` 一样要枚举每条记录——受限的是返回集不是磁盘
    /// 遍历，见 V11 R7-d 的遗留限制。
    pub fn count_parked(&self) -> Result<u64, StorageError> {
        self.ensure_dirs()?;
        let mut count = 0_u64;
        for path in self.list_records()? {
            let event: OutboxEvent = read_state_json(&path)?;
            if crate::outbox_exhausted(event.attempts) {
                count = count.saturating_add(1);
            }
        }
        Ok(count)
    }

    pub fn claim(
        &self,
        event_id: &str,
        owner: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<OutboxLease, StorageError> {
        validate_outbox_name(event_id, "event_id")?;
        if owner.trim().is_empty() || lease_seconds == 0 {
            return Err(StorageError::Conflict(
                "Outbox worker 和租约时长不能为空".into(),
            ));
        }
        self.ensure_dirs()?;
        let _lock = acquire_storage_lock(self.lock_path(event_id)?)?;
        if !self.event_path(event_id)?.exists() {
            return Err(StorageError::NotFound(format!(
                "Outbox event_id {event_id}"
            )));
        }
        let lease_path = self.lease_path(event_id)?;
        let mut fencing_token = 1;
        if lease_path.exists() {
            let current: OutboxLease = read_state_json(&lease_path)?;
            if current.expires_ts > now && current.owner != owner {
                return Err(StorageError::LeaseHeld {
                    run_id: 0,
                    owner: current.owner,
                });
            }
            if current.expires_ts <= now {
                fencing_token = current.fencing_token.saturating_add(1).max(1);
                self.delete_record(&lease_path)?;
            } else {
                fencing_token = current.fencing_token.max(1);
            }
        }
        let lease = OutboxLease {
            event_id: event_id.into(),
            owner: owner.into(),
            expires_ts: now.saturating_add(lease_seconds),
            fencing_token,
        };
        write_state_json(&self.root, &lease_path, &lease)?;
        Ok(lease)
    }

    pub fn ack(
        &self,
        event_id: &str,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<(), StorageError> {
        let _lock = acquire_storage_lock(self.lock_path(event_id)?)?;
        let lease = read_state_json::<OutboxLease>(&self.lease_path(event_id)?)?;
        validate_outbox_lease(&lease, event_id, owner, fencing_token, now)?;
        self.delete_record(&self.event_path(event_id)?)?;
        self.delete_record(&self.lease_path(event_id)?)?;
        Ok(())
    }

    pub fn retry(
        &self,
        event_id: &str,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<(), StorageError> {
        let _lock = acquire_storage_lock(self.lock_path(event_id)?)?;
        let lease = read_state_json::<OutboxLease>(&self.lease_path(event_id)?)?;
        validate_outbox_lease(&lease, event_id, owner, fencing_token, now)?;
        let path = self.event_path(event_id)?;
        let mut event: OutboxEvent = read_state_json(&path)?;
        // P1c（§4.9）：尝试计数只走 `qx-core` 统一策略的递增口径，饱和不溢出。
        event.attempts = retry::RetryPolicy::next_attempt_count(event.attempts);
        write_state_json(&self.root, &path, &event)?;
        self.delete_record(&self.lease_path(event_id)?)?;
        Ok(())
    }

    fn ensure_dirs(&self) -> Result<(), StorageError> {
        for path in ["outbox/events", "outbox/leases", "outbox/locks"] {
            std::fs::create_dir_all(self.root.join(path))
                .map_err(|error| StorageError::Io(error.to_string()))?;
        }
        Ok(())
    }

    fn event_path(&self, event_id: &str) -> Result<PathBuf, StorageError> {
        validate_outbox_name(event_id, "event_id")?;
        Ok(self
            .root
            .join("outbox/events")
            .join(format!("{}.json", outbox_file_key(event_id))))
    }

    fn lease_path(&self, event_id: &str) -> Result<PathBuf, StorageError> {
        validate_outbox_name(event_id, "event_id")?;
        Ok(self
            .root
            .join("outbox/leases")
            .join(format!("{}.json", outbox_file_key(event_id))))
    }

    fn lock_path(&self, event_id: &str) -> Result<PathBuf, StorageError> {
        validate_outbox_name(event_id, "event_id")?;
        Ok(self
            .root
            .join("outbox/locks")
            .join(format!("{}.lock", outbox_file_key(event_id))))
    }
}

impl OutboxStore for FileOutboxStore {
    fn append_outbox(&self, event: OutboxEvent) -> Result<(), StorageError> {
        self.append(event)
    }

    fn available_outbox(&self, now: u64, limit: usize) -> Result<Vec<OutboxEvent>, StorageError> {
        self.available(now, limit)
    }

    fn count_parked_outbox(&self) -> Result<u64, StorageError> {
        self.count_parked()
    }

    fn claim_outbox(
        &self,
        event_id: &str,
        owner: &str,
        now: u64,
        lease_seconds: u64,
    ) -> Result<OutboxLease, StorageError> {
        self.claim(event_id, owner, now, lease_seconds)
    }

    fn ack_outbox(
        &self,
        event_id: &str,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<(), StorageError> {
        self.ack(event_id, owner, fencing_token, now)
    }

    fn retry_outbox(
        &self,
        event_id: &str,
        owner: &str,
        fencing_token: u64,
        now: u64,
    ) -> Result<(), StorageError> {
        self.retry(event_id, owner, fencing_token, now)
    }
}
