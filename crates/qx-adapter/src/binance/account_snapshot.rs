//! Binance Spot account balances, account-position events, and account-wide open-order observations.
//!
//! Snapshots update only the connector's account cache; order state remains owned by the venue.

use super::*;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BinanceBalance {
    pub asset: String,
    pub free: Money,
    pub locked: Money,
    pub free_exact: ExactDecimal,
    pub locked_exact: ExactDecimal,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct BinanceOpenOrderWire {
    pub(super) symbol: String,
    #[serde(rename = "orderId")]
    pub(super) order_id: u64,
    pub(super) side: String,
    #[serde(rename = "origQty")]
    pub(super) original_quantity: String,
    #[serde(rename = "executedQty")]
    pub(super) executed_quantity: String,
}

#[derive(Clone, Debug, Deserialize)]
struct BinanceAccountWire {
    #[serde(default)]
    balances: Vec<BinanceBalanceWire>,
}

#[derive(Clone, Debug, Deserialize)]
struct BinanceBalanceWire {
    asset: String,
    free: String,
    locked: String,
}

impl BinanceSpotVenue {
    pub fn balances(&self) -> Vec<BinanceBalance> {
        self.balances.values().cloned().collect()
    }

    pub fn fetch_account_balances(&mut self, ts: u64) -> QxResult<Vec<BinanceBalance>> {
        // A failed account read is retryable observation failure, not an unknown order result.
        let response = self.request_readonly_with_weight("/api/v3/account", ts, 20)?;
        let wire: BinanceAccountWire = parse_success(response, "account")?;
        let balances = wire
            .balances
            .into_iter()
            .map(|balance| {
                let free_exact = parse_exact_decimal(&balance.free, "free balance")?;
                let locked_exact = parse_exact_decimal(&balance.locked, "locked balance")?;
                Ok(BinanceBalance {
                    asset: balance.asset,
                    free: Money::from_raw(exact_decimal_projection(free_exact)?),
                    locked: Money::from_raw(exact_decimal_projection(locked_exact)?),
                    free_exact,
                    locked_exact,
                })
            })
            .collect::<QxResult<Vec<_>>>()?;
        self.balances = balances
            .iter()
            .cloned()
            .map(|balance| (balance.asset.clone(), balance))
            .collect();
        Ok(balances)
    }

    pub(super) fn fetch_open_orders_for_max_position(
        &mut self,
        now_ms: u64,
    ) -> QxResult<Vec<BinanceOpenOrderWire>> {
        let response = self.request_readonly_with_weight("/api/v3/openOrders", now_ms, 80)?;
        parse_success(response, "MAX_POSITION openOrders")
    }

    pub(super) fn ingest_account_position(&mut self, value: &Value) -> QxResult<()> {
        let event_ts = value
            .get("E")
            .and_then(Value::as_u64)
            .or_else(|| value.get("u").and_then(Value::as_u64))
            .unwrap_or(self.last_event_ts);
        let balances = value
            .get("B")
            .and_then(Value::as_array)
            .ok_or_else(|| QxError::ReconcileRequired("Binance 账户事件缺少 B balances".into()))?;
        for balance in balances {
            let asset = balance
                .get("a")
                .and_then(Value::as_str)
                .ok_or_else(|| QxError::ReconcileRequired("Binance 账户事件缺少 asset".into()))?;
            let free = json_string(balance, "f")?;
            let locked = json_string(balance, "l")?;
            let free_exact = parse_exact_decimal(&free, "event free balance")?;
            let locked_exact = parse_exact_decimal(&locked, "event locked balance")?;
            let snapshot = BinanceBalance {
                asset: asset.into(),
                free: Money::from_raw(exact_decimal_projection(free_exact)?),
                locked: Money::from_raw(exact_decimal_projection(locked_exact)?),
                free_exact,
                locked_exact,
            };
            self.balances.insert(asset.into(), snapshot);
        }
        self.last_event_ts = self.last_event_ts.max(event_ts);
        Ok(())
    }
}
