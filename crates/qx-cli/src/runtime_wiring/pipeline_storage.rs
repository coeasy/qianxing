//! EventLog 管道的打开：按运行时配置在 PostgreSQL / SQLite / 文件（含分段）之间选后端，
//! 并按调用处声明的面决定要不要补投影。选定数据库后端前还要过一道换后端闸门：
//! `storage.root` 里若留着同一账户的文件后端历史，那次切换会让账户归零。
//!
//! 由 `runtime_wiring.rs` 的职责簇拆出（第十七遍 #169：读面不再付写面的代价），
//! 条目经父模块的 `pub(crate) use` 再导出，行为与拆分前逐字相同。

use crate::*;

#[derive(Clone, Debug)]
pub(crate) struct PipelineStorage {
    pub(crate) root: PathBuf,
    segment_events: Option<usize>,
    postgres_dsn: Option<String>,
    sqlite_db: Option<PathBuf>,
    #[cfg(feature = "postgres")]
    postgres_pool_size: usize,
}

impl PipelineStorage {
    pub(crate) fn from_config(config: &RuntimeConfig) -> Result<Self, String> {
        Ok(Self {
            root: Path::new(&config.storage.data_dir).to_path_buf(),
            segment_events: config.storage.event_log_segment_events,
            postgres_dsn: configured_postgres_dsn(config)?,
            sqlite_db: configured_sqlite_event_log(config)?,
            #[cfg(feature = "postgres")]
            postgres_pool_size: config.storage.postgres_pool_size,
        })
    }

    pub(crate) fn open(
        &self,
        log_name: impl Into<String>,
        currency: impl Into<String>,
    ) -> Result<LiveEventPipeline, String> {
        self.open_with(log_name, currency, OutboxRecovery::ReprojectOnOpen)
    }

    /// 读模型的打开：后端选择与 [`Self::open`] 完全一致，但打开过程不写任何东西。
    /// 走 `open` 会让每次 GET 都把整本日志重新投影进 Outbox（文件后端是每事件一把
    /// 全局锁 + 一个文件），把一次读变成一次写，还会把已经 ack 的事实放回待投递队列。
    pub(crate) fn open_read_only(
        &self,
        log_name: impl Into<String>,
        currency: impl Into<String>,
    ) -> Result<LiveEventPipeline, String> {
        self.open_with(log_name, currency, OutboxRecovery::ReadOnly)
    }

    pub(crate) fn open_with(
        &self,
        log_name: impl Into<String>,
        currency: impl Into<String>,
        recovery: OutboxRecovery,
    ) -> Result<LiveEventPipeline, String> {
        let log_name = log_name.into();
        if self.postgres_dsn.is_some() || self.sqlite_db.is_some() {
            // 数据库后端读不到 `storage.root` 里的事件文件：从文件后端换过来之后，那份
            // 旧历史没有读者，而数据库里的空表被当作首次启动，账户快照当场归零。
            // 文件后端之间的同一件事由 `open*` 自己拦（#225），这里补上数据库一侧。
            LiveEventPipeline::assert_no_abandoned_file_log(self.root.clone(), log_name.clone())
                .map_err(|error| format!("EventLog 后端切换被拒绝: {error:?}"))?;
        }
        if let Some(dsn) = self.postgres_dsn.as_deref() {
            #[cfg(not(feature = "postgres"))]
            {
                let _ = dsn;
                return Err(
                    "当前 qx-cli 未启用 postgres feature，无法打开 PostgreSQL EventLog".into(),
                );
            }
            #[cfg(feature = "postgres")]
            {
                let opened = match recovery {
                    OutboxRecovery::ReprojectOnOpen => {
                        LiveEventPipeline::open_postgres_with_pool_size(
                            dsn,
                            self.postgres_pool_size,
                            log_name,
                            currency,
                        )
                    }
                    OutboxRecovery::ReadOnly => LiveEventPipeline::open_postgres_read_only(
                        dsn,
                        self.postgres_pool_size,
                        log_name,
                        currency,
                    ),
                };
                return opened.map_err(|error| format!("打开 PostgreSQL EventLog 失败: {error:?}"));
            }
        }
        if let Some(db) = self.sqlite_db.as_deref() {
            #[cfg(not(feature = "sqlite"))]
            {
                let _ = db;
                return Err("当前 qx-cli 未启用 sqlite feature，无法打开 SQLite EventLog".into());
            }
            #[cfg(feature = "sqlite")]
            {
                let opened = match recovery {
                    OutboxRecovery::ReprojectOnOpen => {
                        LiveEventPipeline::open_sqlite(db, log_name, currency)
                    }
                    OutboxRecovery::ReadOnly => {
                        LiveEventPipeline::open_sqlite_read_only(db, log_name, currency)
                    }
                };
                return opened.map_err(|error| format!("打开 SQLite EventLog 失败: {error:?}"));
            }
        }
        let opened = match recovery {
            OutboxRecovery::ReprojectOnOpen => LiveEventPipeline::open_configured(
                self.root.clone(),
                log_name,
                currency,
                self.segment_events,
            ),
            OutboxRecovery::ReadOnly => LiveEventPipeline::open_read_only(
                self.root.clone(),
                log_name,
                currency,
                self.segment_events,
            ),
        };
        opened.map_err(|error| format!("打开运行时 EventLog 失败: {error:?}"))
    }
}
