//! 公共 Python CCXT 的 Rust 进程桥接。
//!
//! 这里不实现任何交易所签名或 REST 路径，只把 `qianxing_ccxt.worker` 的
//! JSONL 协议接入既有 `Venue` 事实边界。提交结果未知时返回 ReconcileRequired，
//! 不自动重试或补单；订单回报通过 `sync_order` 显式进入统一 VenueEvent。

use qx_core::{
    Fill, MarginMode, Money, Order, OrderStatus, PositionMode, PositionSide, Price, Quantity,
    QxError, QxResult, SCALE,
};
use qx_zhenlu::{
    AdapterHealth, ConnectorCapabilities, ConnectorState, Venue, VenueAdapter, VenueEvent,
    VenueOrderSnapshot,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

pub trait CcxtRpc: Send {
    fn call(&mut self, request: Value) -> Result<Value, String>;
}

pub struct CcxtProcessClient {
    child: Child,
    stdin: ChildStdin,
    responses: Receiver<Result<String, String>>,
    timeout_ms: u64,
}

impl CcxtProcessClient {
    pub fn spawn(
        python: &str,
        config_path: &str,
        working_dir: Option<&str>,
    ) -> Result<Self, String> {
        let child_env = ccxt_worker_environment(config_path)?;
        let timeout_ms = ccxt_worker_timeout_ms(config_path)?;
        let mut command = Command::new(python);
        command
            .args(["-m", "qianxing_ccxt.worker", "--config", config_path])
            .env_clear()
            .envs(child_env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(working_dir) = working_dir {
            command.current_dir(working_dir);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("启动 CCXT Worker 失败: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "CCXT Worker stdin 不可用".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "CCXT Worker stdout 不可用".to_string())?;
        let (sender, responses) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        let _ = sender.send(Err("CCXT Worker 已退出，提交结果未知".into()));
                        return;
                    }
                    Ok(_) => {
                        if sender.send(Ok(line)).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(format!("读取 CCXT Worker 失败: {error}")));
                        return;
                    }
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            responses,
            timeout_ms,
        })
    }
}

fn ccxt_worker_environment(config_path: &str) -> Result<BTreeMap<String, String>, String> {
    let config = ccxt_worker_config(config_path)?;
    let mut environment = BTreeMap::new();
    for key in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP", "PYTHONPATH"] {
        if let Ok(value) = env::var(key) {
            environment.insert(key.to_string(), value);
        }
    }
    let credentials = config
        .get("credential_env")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for field in ["api_key", "secret", "password", "uid"] {
        let Some(variable) = credentials.get(field).and_then(Value::as_str) else {
            continue;
        };
        if variable.trim().is_empty()
            || variable.contains('=')
            || variable.contains('\n')
            || variable.contains('\r')
        {
            return Err(format!("CCXT credential_env.{field} 变量名非法"));
        }
        if let Ok(value) = env::var(variable) {
            environment.insert(variable.to_string(), value);
        }
    }
    Ok(environment)
}

fn ccxt_worker_timeout_ms(config_path: &str) -> Result<u64, String> {
    let config = ccxt_worker_config(config_path)?;
    let timeout_ms = config
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30_000);
    if timeout_ms == 0 || timeout_ms > 300_000 {
        return Err("CCXT timeout_ms 必须在 1..=300000 内".into());
    }
    Ok(timeout_ms)
}

fn ccxt_worker_config(config_path: &str) -> Result<Value, String> {
    let payload = fs::read_to_string(config_path)
        .map_err(|error| format!("读取 CCXT worker 配置失败: {error}"))?;
    serde_json::from_str(&payload).map_err(|error| format!("解析 CCXT worker 配置失败: {error}"))
}

impl CcxtRpc for CcxtProcessClient {
    fn call(&mut self, request: Value) -> Result<Value, String> {
        let payload = serde_json::to_string(&request)
            .map_err(|error| format!("编码 CCXT Worker 请求失败: {error}"))?;
        self.stdin
            .write_all(payload.as_bytes())
            .and_then(|_| self.stdin.write_all(b"\n"))
            .and_then(|_| self.stdin.flush())
            .map_err(|error| format!("写入 CCXT Worker 失败: {error}"))?;
        let line = match self
            .responses
            .recv_timeout(Duration::from_millis(self.timeout_ms))
        {
            Ok(result) => result?,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return Err(format!(
                    "CCXT Worker 响应超时 timeout_ms={}，提交结果未知",
                    self.timeout_ms
                ));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("CCXT Worker 响应通道已断开，提交结果未知".into())
            }
        };
        let response: Value = serde_json::from_str(&line)
            .map_err(|error| format!("解析 CCXT Worker 响应失败: {error}"))?;
        if response.get("ok").and_then(Value::as_bool) != Some(true) {
            let error = response.get("error").cloned().unwrap_or_else(|| json!({}));
            let class = error
                .get("class")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("CCXT Worker 返回失败");
            return Err(format!("CCXT Worker [{class}] {message}"));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| "CCXT Worker 成功响应缺少 result".to_string())
    }
}

impl Drop for CcxtProcessClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct CcxtProcessVenue {
    id: String,
    rpc: Box<dyn CcxtRpc>,
    connected: bool,
    orders: BTreeMap<u64, Order>,
    remote_ids: BTreeMap<u64, String>,
    seen_trade_ids: BTreeMap<u64, BTreeSet<String>>,
    cumulative_costs: BTreeMap<u64, i128>,
}

impl CcxtProcessVenue {
    pub fn new(id: impl Into<String>, rpc: Box<dyn CcxtRpc>) -> Self {
        Self {
            id: id.into(),
            rpc,
            connected: true,
            orders: BTreeMap::new(),
            remote_ids: BTreeMap::new(),
            seen_trade_ids: BTreeMap::new(),
            cumulative_costs: BTreeMap::new(),
        }
    }

    pub fn restore_order(&mut self, order: Order, remote_id: impl Into<String>) -> QxResult<()> {
        order.validate().map_err(QxError::BusinessViolation)?;
        let remote_id = remote_id.into();
        if remote_id.trim().is_empty() {
            return Err(QxError::ReconcileRequired(
                "CCXT 恢复订单缺少 remote order id".into(),
            ));
        }
        let same_remote_order = self
            .remote_ids
            .get(&order.client_id)
            .is_some_and(|existing| existing == &remote_id);
        self.remote_ids.insert(order.client_id, remote_id);
        if !same_remote_order {
            self.cumulative_costs.remove(&order.client_id);
        }
        self.orders.insert(order.client_id, order);
        Ok(())
    }

    /// 拉取订单事实并生成增量成交/取消事件。该方法可由 REST 轮询或 CCXT Pro
    /// 用户流 worker 调用，二者都复用同一事实转换。
    pub fn sync_order(&mut self, client_order_id: u64, ts: u64) -> QxResult<Vec<VenueEvent>> {
        let order = self
            .orders
            .get(&client_order_id)
            .cloned()
            .ok_or_else(|| QxError::Permanent("CCXT 本地订单不存在".into()))?;
        let remote_id = self
            .remote_ids
            .get(&client_order_id)
            .cloned()
            .ok_or_else(|| QxError::ReconcileRequired("CCXT 订单缺少 remote order id".into()))?;
        let result = self.call_or_reconcile(json!({
            "op": "fetch_order",
            "order_id": remote_id,
            "instrument": order.instrument.to_string(),
        }))?;
        let remote = result
            .get("order")
            .ok_or_else(|| QxError::ReconcileRequired("CCXT fetch_order 缺少 order".into()))?;
        let filled = raw_i128(remote, "filled_raw")?;
        if filled < order.filled.raw() || filled > order.qty.raw() {
            return Err(QxError::ReconcileRequired(
                "CCXT filled 与本地订单不一致".into(),
            ));
        }
        let mut events = Vec::new();
        let prior_filled = order.filled.raw();
        let prior_cost = self
            .cumulative_costs
            .get(&client_order_id)
            .copied()
            .unwrap_or(0);
        if filled > prior_filled
            && prior_filled > 0
            && !self.cumulative_costs.contains_key(&client_order_id)
        {
            return Err(QxError::ReconcileRequired(
                "CCXT 增量成交缺少上一阶段累计成本，禁止按全量均价伪造增量价格".into(),
            ));
        }
        let cumulative_cost = if filled > prior_filled {
            cumulative_cost_raw(remote, filled)?
        } else if filled == 0 {
            0
        } else if remote.get("cost_raw").is_some()
            || remote.get("average_raw").is_some()
            || remote.get("price_raw").is_some()
        {
            cumulative_cost_raw(remote, filled)?
        } else {
            prior_cost
        };
        if cumulative_cost < prior_cost {
            return Err(QxError::ReconcileRequired(
                "CCXT 累计成交成本不能回退".into(),
            ));
        }
        if filled > order.filled.raw() {
            let delta_qty = filled - prior_filled;
            let delta_cost = cumulative_cost - prior_cost;
            if delta_qty <= 0 || delta_cost <= 0 {
                return Err(QxError::ReconcileRequired(
                    "CCXT 增量成交缺少有效累计成本".into(),
                ));
            }
            let price = delta_cost
                .checked_mul(SCALE)
                .and_then(|value| value.checked_div(delta_qty))
                .filter(|price| *price > 0)
                .ok_or_else(|| QxError::ReconcileRequired("CCXT 增量成交价格无效".into()))?;
            let mut fee_raw = remote
                .get("fee_raw")
                .map(|value| raw_i128_value(value, "fee_raw"))
                .transpose()?
                .unwrap_or(0);
            if fee_raw < 0 {
                return Err(QxError::ReconcileRequired("CCXT 成交费用不能为负".into()));
            }
            let mut fee_currency = remote
                .get("fee_currency")
                .and_then(Value::as_str)
                .map(str::to_string);
            if fee_raw == 0 {
                let (trade_fee, trade_currency) = self.incremental_trade_fee(
                    client_order_id,
                    &order.instrument.to_string(),
                    &remote_id,
                )?;
                fee_raw = trade_fee;
                if fee_currency.is_none() {
                    fee_currency = trade_currency;
                }
            }
            let mut fill = Fill {
                order_id: client_order_id,
                qty: Quantity::from_raw(delta_qty),
                price: Price::from_raw(price),
                fee: Money::from_raw(fee_raw),
                ts,
                account_id: order.account_id.clone(),
                strategy_id: None,
                signal_id: None,
                intent_id: None,
                venue_id: Some(self.id.clone()),
                venue_order_id: Some(remote_id.clone()),
                rule_version: None,
                fee_currency,
            };
            order.trace_fill(&mut fill, Some(&self.id), Some(&remote_id));
            events.push(VenueEvent::Fill(fill));
        }
        self.cumulative_costs
            .insert(client_order_id, cumulative_cost);
        let remote_status = remote
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        match remote_status {
            "canceled" | "cancelled" | "expired" if !order.status.is_terminal() => {
                events.push(VenueEvent::Cancelled {
                    client_order_id,
                    ts,
                })
            }
            "rejected" => {
                return Err(QxError::Permanent("CCXT 订单被交易所拒绝".into()));
            }
            _ => {}
        }
        if let Some(local) = self.orders.get_mut(&client_order_id) {
            local.filled = Quantity::from_raw(filled);
            if filled == local.qty.raw() {
                local.status = OrderStatus::Filled;
            } else if filled > 0 {
                local.status = OrderStatus::PartiallyFilled;
            } else if matches!(remote_status, "canceled" | "cancelled" | "expired") {
                local.status = OrderStatus::Cancelled;
            }
        }
        Ok(events)
    }

    fn call_or_reconcile(&mut self, request: Value) -> QxResult<Value> {
        self.rpc
            .call(request)
            .map_err(|error| QxError::ReconcileRequired(format!("CCXT Worker 结果未知: {error}")))
    }

    /// 将 CCXT Pro/扩展 JSONL 事件请求保持在同一进程边界；调用方只能得到
    /// 原始观察结果，订单事实仍必须通过 `sync_order` 和 Runtime 归约。
    pub fn stream_call(&mut self, request: Value) -> QxResult<Value> {
        self.call_or_reconcile(request)
    }

    /// 连接断开后替换 JSONL RPC 进程；本地 remote order/成交去重状态保留，
    /// 重新连接不代表重新下单。
    pub fn replace_rpc(&mut self, rpc: Box<dyn CcxtRpc>) {
        self.rpc = rpc;
        self.connected = true;
    }

    fn incremental_trade_fee(
        &mut self,
        client_order_id: u64,
        instrument: &str,
        remote_order_id: &str,
    ) -> QxResult<(i128, Option<String>)> {
        let result = match self.rpc.call(json!({
            "op": "fetch_my_trades",
            "instrument": instrument,
            "params": {"order": remote_order_id},
        })) {
            Ok(result) => result,
            Err(error) if error.contains("[unsupported]") => return Ok((0, None)),
            Err(error) => {
                return Err(QxError::ReconcileRequired(format!(
                    "CCXT 成交费用查询结果未知: {error}"
                )))
            }
        };
        let trades = result
            .get("trades")
            .and_then(Value::as_array)
            .ok_or_else(|| QxError::ReconcileRequired("CCXT trades 响应缺少 trades".into()))?;
        let seen = self.seen_trade_ids.entry(client_order_id).or_default();
        let mut fee = 0_i128;
        let mut currency = None;
        for trade in trades {
            let Some(trade_id) = trade.get("trade_id").and_then(Value::as_str) else {
                return Err(QxError::ReconcileRequired(
                    "CCXT trade 缺少 trade_id".into(),
                ));
            };
            if !seen.insert(trade_id.to_string()) {
                continue;
            }
            let amount = raw_i128(trade, "fee_raw")?;
            if amount < 0 {
                return Err(QxError::ReconcileRequired("CCXT trade fee 不能为负".into()));
            }
            fee = fee
                .checked_add(amount)
                .ok_or_else(|| QxError::Invariant("CCXT 成交费用累计溢出".into()))?;
            if currency.is_none() {
                currency = trade
                    .get("fee_currency")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
        }
        Ok((fee, currency))
    }
}

impl Venue for CcxtProcessVenue {
    fn id(&self) -> &str {
        &self.id
    }

    fn submit(&mut self, order: Order, ts: u64) -> QxResult<Vec<VenueEvent>> {
        if !self.connected {
            return Err(QxError::VenueState("CCXT Worker 未连接".into()));
        }
        let order_type = if order.limit.is_some() {
            "limit"
        } else {
            "market"
        };
        let mut params = serde_json::Map::new();
        if let Some(policy) = order.policy {
            if policy.leverage == 0 {
                return Err(QxError::BusinessViolation("CCXT 订单杠杆必须大于 0".into()));
            }
            if policy.margin_mode == MarginMode::Cash && policy.leverage != 1 {
                return Err(QxError::BusinessViolation(
                    "CCXT Cash 订单只能使用 1x 杠杆".into(),
                ));
            }
            if policy.margin_mode != MarginMode::Cash {
                self.call_or_reconcile(json!({
                    "op": "set_margin_mode",
                    "instrument": order.instrument.to_string(),
                    "margin_mode": match policy.margin_mode {
                        MarginMode::Cross => "cross",
                        MarginMode::Isolated => "isolated",
                        MarginMode::Cash => "cash",
                    },
                }))?;
            }
            if policy.leverage != 1 {
                self.call_or_reconcile(json!({
                    "op": "set_leverage",
                    "instrument": order.instrument.to_string(),
                    "leverage": policy.leverage,
                    "margin_mode": match policy.margin_mode {
                        MarginMode::Cross => "cross",
                        MarginMode::Isolated => "isolated",
                        MarginMode::Cash => "cash",
                    },
                }))?;
            }
            if policy.position_mode == PositionMode::Hedge || policy.margin_mode != MarginMode::Cash
            {
                self.call_or_reconcile(json!({
                    "op": "set_position_mode",
                    "hedged": policy.position_mode == PositionMode::Hedge,
                    "instrument": order.instrument.to_string(),
                }))?;
            }
            if policy.reduce_only {
                params.insert("reduceOnly".into(), Value::Bool(true));
            }
            if policy.post_only {
                params.insert("postOnly".into(), Value::Bool(true));
            }
            match policy.position_side {
                PositionSide::Long => {
                    params.insert("positionSide".into(), Value::String("long".into()));
                }
                PositionSide::Short => {
                    params.insert("positionSide".into(), Value::String("short".into()));
                }
                PositionSide::Net => {}
            }
        }
        let result = self.call_or_reconcile(json!({
            "op": "create_order",
            "instrument": order.instrument.to_string(),
            "side": if matches!(order.side, qx_core::Side::Buy) { "buy" } else { "sell" },
            "order_type": order_type,
            "amount_raw": order.qty.raw(),
            "price_raw": order.limit.map(|price| price.raw()),
            "client_order_id": order.client_id.to_string(),
            "params": params,
        }))?;
        let remote = result
            .get("order")
            .ok_or_else(|| QxError::ReconcileRequired("CCXT create_order 缺少 order".into()))?;
        let remote_id = remote
            .get("order_id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| QxError::ReconcileRequired("CCXT create_order 缺少 order_id".into()))?
            .to_string();
        self.remote_ids.insert(order.client_id, remote_id.clone());
        self.cumulative_costs.remove(&order.client_id);
        self.orders.insert(order.client_id, order.clone());
        Ok(vec![VenueEvent::Accepted {
            client_order_id: order.client_id,
            venue_order_id: remote_id,
            ts,
        }])
    }

    fn cancel(&mut self, client_order_id: u64, ts: u64) -> QxResult<Vec<VenueEvent>> {
        let order = self
            .orders
            .get(&client_order_id)
            .ok_or_else(|| QxError::Permanent("CCXT 本地订单不存在".into()))?;
        let remote_id = self
            .remote_ids
            .get(&client_order_id)
            .ok_or_else(|| QxError::ReconcileRequired("CCXT 订单缺少 remote order id".into()))?;
        self.call_or_reconcile(json!({
            "op": "cancel_order",
            "order_id": remote_id,
            "instrument": order.instrument.to_string(),
        }))?;
        if let Some(local) = self.orders.get_mut(&client_order_id) {
            local.status = OrderStatus::Cancelled;
        }
        Ok(vec![VenueEvent::Cancelled {
            client_order_id,
            ts,
        }])
    }

    fn snapshot(&self) -> Vec<VenueOrderSnapshot> {
        self.orders
            .values()
            .map(|order| VenueOrderSnapshot {
                client_order_id: order.client_id,
                status: order.status,
                filled: order.filled,
            })
            .collect()
    }

    fn connected(&self) -> bool {
        self.connected
    }
}

impl VenueAdapter for CcxtProcessVenue {
    fn capabilities(&self) -> ConnectorCapabilities {
        ConnectorCapabilities {
            market_data: true,
            // 当前进程边界只实现公共 CCXT REST；CCXT Pro watch_* 必须显式
            // 注入独立流 worker，不能把 REST 轮询冒充用户流能力。
            user_stream: false,
            submit: true,
            cancel: true,
            replace: false,
        }
    }

    fn health(&self) -> AdapterHealth {
        AdapterHealth {
            state: if self.connected {
                ConnectorState::Live
            } else {
                ConnectorState::ReconcileRequired
            },
            last_event_ts: 0,
            reconnects: 0,
        }
    }
}

fn raw_i128(value: &Value, key: &str) -> QxResult<i128> {
    value.get(key).map_or_else(
        || {
            Err(QxError::ReconcileRequired(format!(
                "CCXT 字段不是定点整数: {key}"
            )))
        },
        |value| raw_i128_value(value, key),
    )
}

fn raw_i128_value(value: &Value, key: &str) -> QxResult<i128> {
    if let Some(value) = value.as_i64() {
        return Ok(i128::from(value));
    }
    if let Some(value) = value.as_u64() {
        return Ok(i128::from(value));
    }
    if let Some(value) = value.as_str() {
        return value
            .parse::<i128>()
            .map_err(|_| QxError::ReconcileRequired(format!("CCXT 字段不是定点整数: {key}")));
    }
    Err(QxError::ReconcileRequired(format!(
        "CCXT 字段不是定点整数: {key}"
    )))
}

fn cumulative_cost_raw(remote: &Value, filled_raw: i128) -> QxResult<i128> {
    if filled_raw <= 0 {
        return Ok(0);
    }
    if let Some(value) = remote.get("cost_raw") {
        let cost = raw_i128_value(value, "cost_raw")?;
        if cost <= 0 {
            return Err(QxError::ReconcileRequired(
                "CCXT 累计成交成本必须为正".into(),
            ));
        }
        return Ok(cost);
    }
    let average = remote
        .get("average_raw")
        .or_else(|| remote.get("price_raw"))
        .map(|value| raw_i128_value(value, "average_raw"))
        .transpose()?
        .filter(|price| *price > 0)
        .ok_or_else(|| QxError::ReconcileRequired("CCXT 成交缺少累计价格".into()))?;
    average
        .checked_mul(filled_raw)
        .and_then(|value| value.checked_div(SCALE))
        .filter(|cost| *cost > 0)
        .ok_or_else(|| QxError::ReconcileRequired("CCXT 累计成交成本无效".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{
        InstrumentId, MarginMode, OrderPolicy, PositionMode, PositionSide, Quantity, Side,
    };
    use std::sync::{Arc, Mutex};

    #[test]
    fn ccxt_worker_environment_is_minimal_and_validates_credential_names() {
        let path =
            std::env::temp_dir().join(format!("qianxing-ccxt-env-{}.json", std::process::id()));
        std::fs::write(
            &path,
            r#"{"credential_env":{"api_key":"QX_TEST_API_KEY","secret":"QX_TEST_SECRET"}}"#,
        )
        .unwrap();
        let environment = ccxt_worker_environment(&path.to_string_lossy()).unwrap();
        assert!(!environment.contains_key("QX_UNRELATED_PARENT_SECRET"));
        let _ = std::fs::write(&path, r#"{"credential_env":{"secret":"BAD=VARIABLE"}}"#);
        assert!(ccxt_worker_environment(&path.to_string_lossy()).is_err());
        let _ = std::fs::remove_file(path);
    }

    struct FakeRpc {
        calls: Vec<Value>,
    }

    impl CcxtRpc for FakeRpc {
        fn call(&mut self, request: Value) -> Result<Value, String> {
            self.calls.push(request.clone());
            match request.get("op").and_then(Value::as_str) {
                Some("create_order") => Ok(json!({
                    "order": {"order_id": "remote-1", "status": "open", "filled_raw": 0}
                })),
                Some("fetch_order") => Ok(json!({
                    "order": {
                        "order_id": "remote-1",
                        "status": "closed",
                        "filled_raw": 2_000_000_000i64,
                        "average_raw": 100_000_000_000i64,
                        "fee_raw": 1_000_000_000i64,
                        "fee_currency": "USDT"
                    }
                })),
                _ => Ok(json!({})),
            }
        }
    }

    fn order() -> Order {
        Order {
            client_id: 7,
            instrument: InstrumentId::parse("BTC/USDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_i64(2),
            limit: Some(Price::from_i64(100)),
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        }
    }

    #[test]
    fn ccxt_process_venue_submits_and_syncs_fill_without_custom_exchange_protocol() {
        let mut venue = CcxtProcessVenue::new("binance", Box::new(FakeRpc { calls: Vec::new() }));
        assert!(matches!(
            venue.submit(order(), 10).unwrap()[0],
            VenueEvent::Accepted { .. }
        ));
        let events = venue.sync_order(7, 11).unwrap();
        match &events[0] {
            VenueEvent::Fill(fill) => {
                assert_eq!(fill.fee, Money::from_i64(1));
                assert_eq!(fill.fee_currency.as_deref(), Some("USDT"));
            }
            other => panic!("expected fill, got {other:?}"),
        }
        assert_eq!(
            venue.snapshot()[0].filled.raw(),
            Quantity::from_i64(2).raw()
        );
    }

    #[test]
    fn ccxt_process_venue_enriches_missing_order_fee_from_incremental_trades() {
        struct TradeFeeRpc;
        impl CcxtRpc for TradeFeeRpc {
            fn call(&mut self, request: Value) -> Result<Value, String> {
                match request.get("op").and_then(Value::as_str) {
                    Some("create_order") => Ok(json!({
                        "order": {"order_id": "remote-fee", "status": "open", "filled_raw": 0}
                    })),
                    Some("fetch_order") => Ok(json!({
                        "order": {
                            "order_id": "remote-fee",
                            "status": "closed",
                            "filled_raw": 2_000_000_000_i64,
                            "average_raw": 100_000_000_000_i64,
                            "fee_raw": 0
                        }
                    })),
                    Some("fetch_my_trades") => Ok(json!({
                        "trades": [{
                            "trade_id": "trade-fee-1",
                            "fee_raw": 2_000_000_000_i64,
                            "fee_currency": "USDT"
                        }]
                    })),
                    _ => Ok(json!({})),
                }
            }
        }
        let mut venue = CcxtProcessVenue::new("okx", Box::new(TradeFeeRpc));
        venue.submit(order(), 10).unwrap();
        let events = venue.sync_order(7, 11).unwrap();
        match &events[0] {
            VenueEvent::Fill(fill) => {
                assert_eq!(fill.fee, Money::from_i64(2));
                assert_eq!(fill.fee_currency.as_deref(), Some("USDT"));
            }
            other => panic!("expected fill, got {other:?}"),
        }
    }

    #[test]
    fn ccxt_partial_sync_uses_incremental_price_from_cumulative_average() {
        struct PartialRpc {
            fetches: usize,
        }

        impl CcxtRpc for PartialRpc {
            fn call(&mut self, request: Value) -> Result<Value, String> {
                match request.get("op").and_then(Value::as_str) {
                    Some("create_order") => Ok(json!({
                        "order": {"order_id": "partial-1", "status": "open", "filled_raw": 0}
                    })),
                    Some("fetch_order") => {
                        self.fetches += 1;
                        if self.fetches == 1 {
                            Ok(json!({
                                "order": {
                                    "order_id": "partial-1",
                                    "status": "open",
                                    "filled_raw": 1_000_000_000_i64,
                                    "average_raw": 100_000_000_000_i64,
                                    "fee_raw": 1
                                }
                            }))
                        } else {
                            Ok(json!({
                                "order": {
                                    "order_id": "partial-1",
                                    "status": "closed",
                                    "filled_raw": 2_000_000_000_i64,
                                    "average_raw": 110_000_000_000_i64,
                                    "fee_raw": 2
                                }
                            }))
                        }
                    }
                    _ => Ok(json!({})),
                }
            }
        }

        let mut venue = CcxtProcessVenue::new("binance", Box::new(PartialRpc { fetches: 0 }));
        venue.submit(order(), 10).unwrap();
        let first = venue.sync_order(7, 11).unwrap();
        let second = venue.sync_order(7, 12).unwrap();
        let VenueEvent::Fill(first_fill) = &first[0] else {
            panic!("expected first partial fill")
        };
        let VenueEvent::Fill(second_fill) = &second[0] else {
            panic!("expected second partial fill")
        };
        assert_eq!(first_fill.qty, Quantity::from_i64(1));
        assert_eq!(first_fill.price, Price::from_i64(100));
        assert_eq!(second_fill.qty, Quantity::from_i64(1));
        assert_eq!(second_fill.price, Price::from_i64(120));
    }

    #[test]
    fn ccxt_order_policy_is_translated_before_create_order() {
        let calls = Arc::new(Mutex::new(Vec::<Value>::new()));
        struct PolicyRpc {
            calls: Arc<Mutex<Vec<Value>>>,
        }
        impl CcxtRpc for PolicyRpc {
            fn call(&mut self, request: Value) -> Result<Value, String> {
                self.calls.lock().unwrap().push(request.clone());
                if request.get("op").and_then(Value::as_str) == Some("create_order") {
                    Ok(json!({
                        "order": {"order_id": "policy-1", "status": "open", "filled_raw": 0}
                    }))
                } else {
                    Ok(json!({}))
                }
            }
        }
        let mut requested = order();
        requested.policy = Some(OrderPolicy {
            reduce_only: true,
            position_side: PositionSide::Long,
            margin_mode: MarginMode::Isolated,
            position_mode: PositionMode::Hedge,
            leverage: 3,
            post_only: true,
        });
        let mut venue = CcxtProcessVenue::new(
            "binance",
            Box::new(PolicyRpc {
                calls: Arc::clone(&calls),
            }),
        );
        venue.submit(requested, 10).unwrap();
        let calls = calls.lock().unwrap();
        assert!(calls.iter().any(|call| call["op"] == "set_margin_mode"));
        assert!(calls.iter().any(|call| call["op"] == "set_leverage"));
        assert!(calls.iter().any(|call| call["op"] == "set_position_mode"));
        let create = calls
            .iter()
            .find(|call| call["op"] == "create_order")
            .unwrap();
        assert_eq!(create["params"]["reduceOnly"], true);
        assert_eq!(create["params"]["postOnly"], true);
        assert_eq!(create["params"]["positionSide"], "long");
    }
}
