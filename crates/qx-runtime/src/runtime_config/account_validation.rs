//! 账户注册表与旧版 worker/strategy 账户字段的绑定校验。

use super::*;

impl RuntimeConfig {
    pub(super) fn validate_accounts(&self) -> Result<(), String> {
        let mut account_ids = std::collections::BTreeSet::new();
        for account in &self.accounts {
            if account.id.trim().is_empty() || !account_ids.insert(account.id.as_str()) {
                return Err(format!("account id 为空或重复: {}", account.id));
            }
            if account
                .settlement_currency
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                return Err(format!(
                    "Account[{}] settlement_currency 不能为空",
                    account.id
                ));
            }
            if account.initial_cash_raw.is_some_and(|value| value <= 0) {
                return Err(format!(
                    "Account[{}] initial_cash_raw 必须为正数",
                    account.id
                ));
            }
        }
        if self.accounts.is_empty() {
            return Ok(());
        }
        for worker in &self.workers {
            if let Some(account_id) = worker.account_id.as_deref() {
                let account = self.account(account_id).ok_or_else(|| {
                    format!("worker {} 引用了未声明的账户 {account_id}", worker.id)
                })?;
                if let (Some(canonical), Some(legacy)) =
                    (account.initial_cash_raw, worker.paper_initial_cash_raw)
                {
                    if canonical != legacy {
                        return Err(format!(
                            "worker {} paper_initial_cash_raw 与 Account[{account_id}].initial_cash_raw 不一致",
                            worker.id
                        ));
                    }
                }
                if let (Some(canonical), Some(legacy)) = (
                    account.settlement_currency.as_deref(),
                    worker.settlement_currency.as_deref(),
                ) {
                    if !canonical.eq_ignore_ascii_case(legacy) {
                        return Err(format!(
                            "worker {} settlement_currency 与 Account[{account_id}] 不一致",
                            worker.id
                        ));
                    }
                }
            }
        }
        for strategy in std::iter::once(&self.strategy).chain(self.strategies.iter()) {
            if let Some(account_id) = strategy.account_id.as_deref() {
                let account = self
                    .account(account_id)
                    .ok_or_else(|| format!("Strategy 引用了未声明的账户 {account_id}"))?;
                if account.initial_cash_raw.is_some_and(|canonical| {
                    strategy
                        .initial_cash_raw
                        .is_some_and(|legacy| legacy != canonical)
                }) {
                    return Err(format!(
                        "Strategy[{account_id}] initial_cash_raw 与账户注册表不一致"
                    ));
                }
            }
        }
        for account in &self.accounts {
            let referenced = self
                .workers
                .iter()
                .any(|worker| worker.account_id.as_deref() == Some(account.id.as_str()))
                || std::iter::once(&self.strategy)
                    .chain(self.strategies.iter())
                    .any(|strategy| strategy.account_id.as_deref() == Some(account.id.as_str()));
            if !referenced {
                return Err(format!(
                    "Account[{}] 没有绑定 worker 或 strategy，属于未接入配置",
                    account.id
                ));
            }
            if account.initial_cash_raw.is_some() {
                let venues = self
                    .workers
                    .iter()
                    .filter(|worker| {
                        worker.enabled
                            && worker.account_id.as_deref() == Some(account.id.as_str())
                            && matches!(
                                worker.role,
                                WorkerRole::UserStream
                                    | WorkerRole::Execution
                                    | WorkerRole::SpreadRecovery
                                    | WorkerRole::Reconciler
                            )
                    })
                    .filter_map(|worker| worker.venue_id.as_deref())
                    .collect::<std::collections::BTreeSet<_>>();
                if venues.len() > 1 {
                    return Err(format!(
                        "Account[{}] 配置了统一初始本金但绑定多个交易所账簿 ({})；请按交易所拆分账户，避免在独立账簿重复注资",
                        account.id,
                        venues.into_iter().collect::<Vec<_>>().join(", ")
                    ));
                }
            }
        }
        Ok(())
    }
}
