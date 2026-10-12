//! Deterministic, bounded allocation of a portfolio cash budget to strategy accounts.
//!
//! Lower numeric priority is served first. Equal-priority requests are ordered by
//! strategy id, so allocation never depends on thread arrival or input ordering.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapitalAllocationAccount {
    pub account_id: String,
    pub available_cash_raw: i128,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapitalAllocationRequest {
    pub strategy_id: String,
    pub account_id: String,
    pub requested_cash_raw: i128,
    pub priority: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapitalAllocationInput {
    pub portfolio_id: String,
    pub portfolio_budget_raw: i128,
    pub accounts: Vec<CapitalAllocationAccount>,
    pub requests: Vec<CapitalAllocationRequest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapitalAllocation {
    pub strategy_id: String,
    pub account_id: String,
    pub priority: u32,
    pub requested_cash_raw: i128,
    pub allocated_cash_raw: i128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapitalAllocationError {
    InvalidInput(String),
    Arithmetic(String),
}

pub fn allocate_portfolio_capital(
    input: &CapitalAllocationInput,
) -> Result<Vec<CapitalAllocation>, CapitalAllocationError> {
    if !valid_identity(&input.portfolio_id) || input.portfolio_budget_raw < 0 {
        return Err(CapitalAllocationError::InvalidInput(
            "portfolio id must be a valid identity token and budget must be non-negative".into(),
        ));
    }
    if input.accounts.len() > 1024 || input.requests.len() > 10_000 {
        return Err(CapitalAllocationError::InvalidInput(
            "allocation is limited to 1024 accounts and 10000 strategy requests".into(),
        ));
    }

    let mut cash_by_account = BTreeMap::new();
    for account in &input.accounts {
        if !valid_identity(&account.account_id) || account.available_cash_raw < 0 {
            return Err(CapitalAllocationError::InvalidInput(
                "account id must be a valid identity token and available cash must be non-negative"
                    .into(),
            ));
        }
        if cash_by_account
            .insert(account.account_id.as_str(), account.available_cash_raw)
            .is_some()
        {
            return Err(CapitalAllocationError::InvalidInput(format!(
                "duplicate account id: {}",
                account.account_id
            )));
        }
    }

    let mut seen_strategies = BTreeSet::new();
    for request in &input.requests {
        if !valid_identity(&request.strategy_id) || request.requested_cash_raw < 0 {
            return Err(CapitalAllocationError::InvalidInput(
                "strategy id must be a valid identity token and requested cash must be non-negative".into(),
            ));
        }
        if !seen_strategies.insert(request.strategy_id.as_str()) {
            return Err(CapitalAllocationError::InvalidInput(format!(
                "duplicate strategy id: {}",
                request.strategy_id
            )));
        }
        if !cash_by_account.contains_key(request.account_id.as_str()) {
            return Err(CapitalAllocationError::InvalidInput(format!(
                "strategy {} references unknown account {}",
                request.strategy_id, request.account_id
            )));
        }
    }

    let mut ordered: Vec<_> = input.requests.iter().collect();
    ordered.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.strategy_id.cmp(&right.strategy_id))
    });

    let mut remaining_by_account = cash_by_account;
    let mut portfolio_remaining = input.portfolio_budget_raw;
    let mut allocations = Vec::with_capacity(ordered.len());
    for request in ordered {
        let account_remaining = remaining_by_account
            .get_mut(request.account_id.as_str())
            .expect("account references validated above");
        let allocated = request
            .requested_cash_raw
            .min(*account_remaining)
            .min(portfolio_remaining);
        *account_remaining = account_remaining
            .checked_sub(allocated)
            .ok_or_else(|| CapitalAllocationError::Arithmetic("account cash underflow".into()))?;
        portfolio_remaining = portfolio_remaining.checked_sub(allocated).ok_or_else(|| {
            CapitalAllocationError::Arithmetic("portfolio budget underflow".into())
        })?;
        allocations.push(CapitalAllocation {
            strategy_id: request.strategy_id.clone(),
            account_id: request.account_id.clone(),
            priority: request.priority,
            requested_cash_raw: request.requested_cash_raw,
            allocated_cash_raw: allocated,
        });
    }
    Ok(allocations)
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        strategy_id: &str,
        account_id: &str,
        requested_cash_raw: i128,
        priority: u32,
    ) -> CapitalAllocationRequest {
        CapitalAllocationRequest {
            strategy_id: strategy_id.into(),
            account_id: account_id.into(),
            requested_cash_raw,
            priority,
        }
    }

    #[test]
    fn allocation_is_order_independent_and_respects_portfolio_and_account_caps() {
        let input = CapitalAllocationInput {
            portfolio_id: "portfolio-a".into(),
            portfolio_budget_raw: 120,
            accounts: vec![
                CapitalAllocationAccount {
                    account_id: "a".into(),
                    available_cash_raw: 100,
                },
                CapitalAllocationAccount {
                    account_id: "b".into(),
                    available_cash_raw: 100,
                },
            ],
            requests: vec![
                request("z", "b", 80, 0),
                request("b", "a", 80, 0),
                request("a", "a", 40, 1),
            ],
        };
        let first = allocate_portfolio_capital(&input).unwrap();
        let mut reversed = input.clone();
        reversed.requests.reverse();
        assert_eq!(first, allocate_portfolio_capital(&reversed).unwrap());
        assert_eq!(
            first
                .iter()
                .map(|item| item.allocated_cash_raw)
                .collect::<Vec<_>>(),
            vec![80, 40, 0]
        );
        assert_eq!(
            first
                .iter()
                .map(|item| item.allocated_cash_raw)
                .sum::<i128>(),
            120
        );
    }

    #[test]
    fn rejects_duplicate_and_unowned_strategy_requests() {
        let base = CapitalAllocationInput {
            portfolio_id: "p".into(),
            portfolio_budget_raw: 10,
            accounts: vec![CapitalAllocationAccount {
                account_id: "a".into(),
                available_cash_raw: 10,
            }],
            requests: vec![request("s", "missing", 1, 0)],
        };
        assert!(matches!(
            allocate_portfolio_capital(&base),
            Err(CapitalAllocationError::InvalidInput(_))
        ));
        let mut duplicate = base;
        duplicate.requests = vec![request("s", "a", 1, 0), request("s", "a", 2, 1)];
        assert!(matches!(
            allocate_portfolio_capital(&duplicate),
            Err(CapitalAllocationError::InvalidInput(_))
        ));
    }
}
