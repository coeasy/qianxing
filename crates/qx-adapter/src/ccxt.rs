//! 公共 Python CCXT 的 Rust 进程桥接。
//!
//! 这里不实现交易所签名或 REST 路径，只把 `qianxing_ccxt.worker` JSONL 接入既有 `Venue` 事实边界。
//! 提交结果未知时要求对账，不自动重试或补单；订单回报经 `sync_order` 明确进入统一 VenueEvent。

pub use crate::ccxt_provider::CcxtBarDataProvider;
use crate::io_budget::{write_all_within, write_all_within_cancellable, CancellableWrite};
use crate::order_lifecycle::OrderLifecycle;
use crate::venue_cache::evict_stale_terminal_orders;
use qx_core::{
    ExactDecimal, Fill, FillFee, MarginMode, Money, Order, OrderPolicy, OrderStatus, PositionMode,
    PositionSide, Price, Quantity, QxError, QxResult, SCALE,
};
use qx_zhenlu::{Venue, VenueEvent, VenueOrderSnapshot};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

mod exact_sync;
mod restore;
use std::env;
use std::fs;
use std::io::BufReader;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};
pub trait CcxtRpc: Send {
    fn call(&mut self, request: Value) -> Result<Value, String>;

    fn call_cancellable(
        &mut self,
        request: Value,
        should_cancel: &mut dyn FnMut() -> bool,
    ) -> Result<CcxtCallOutcome, String> {
        if should_cancel() {
            return Ok(CcxtCallOutcome::Cancelled);
        }
        self.call(request).map(CcxtCallOutcome::Response)
    }
}

#[derive(Debug, PartialEq)]
pub enum CcxtCallOutcome {
    Response(Value),
    Cancelled,
}

pub struct CcxtProcessClient {
    child: Child,
    stdin: Option<ChildStdin>,
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
        let child = command
            .spawn()
            .map_err(|error| format!("启动 CCXT Worker 失败: {python} 无法执行: {error}"))?;
        Self::from_child(child, timeout_ms, python)
    }

    fn from_child(mut child: Child, timeout_ms: u64, worker_program: &str) -> Result<Self, String> {
        // `take()` 失败要先把子进程收掉再返回：`Stdio::piped()` 之后这两个 `Some` 是
        // 不变式，此路径今天走不到，但上游一旦把 worker 改成无管道模式，这条 `?`
        // 就会留下一个无人回收的孤儿进程。与 `strategy_host.rs` 的同一处防御对齐
        // （V13 R12）。
        let stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("CCXT Worker stdin 不可用".to_string());
            }
        };
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("CCXT Worker stdout 不可用".to_string());
            }
        };
        // 队列有界：泵线程读子进程 stdout，每行都往通道里塞，而调用侧每次只取一行。
        // 无界通道让杂印与迟到的应答一路攒下去；改成有界之后泵在写满时被顶住，
        // 背压回到子进程的 stdout 管道，内存不再随运行时长增长（V13 R1-D）。
        let (sender, responses) = mpsc::sync_channel(1);
        let worker_program = worker_program.to_string();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                // 单行有字节界：`read_line` 会一直往同一个 `String` 里长，子进程少写一个换行符
                // 就能把本进程吃到内存耗尽。越界与 EOF 都必须让调用侧看见——静默丢掉一行，
                // 等于把"应答没读到"说成"worker 什么都没发生"。
                match crate::read_capped_line(&mut reader, crate::MAX_WORKER_LINE_BYTES) {
                    Ok(None) => {
                        // "提交结果未知"是安全语义，不能被诊断文本改掉；这里只追加死掉的是哪个程序。
                        let _ = sender.send(Err(format!(
                            "CCXT Worker 已退出，提交结果未知（程序={worker_program}）"
                        )));
                        return;
                    }
                    Ok(Some(line)) => {
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
            stdin: Some(stdin),
            responses,
            timeout_ms,
        })
    }

    /// 这条 RPC 通道的读窗超时：`watch_*` 调用方据此给子进程一个更小的
    /// `wait_ms`，让"这一窗没有事件"的回话先于读窗到期到达。
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    /// Send one request while observing the owning worker's shutdown token during both IPC phases.
    /// A cancelled in-flight request is ambiguous, so the subprocess is killed and the result is
    /// reported as cancellation; callers must not retry an order automatically.
    pub fn call_cancellable(
        &mut self,
        request: Value,
        mut should_cancel: impl FnMut() -> bool,
    ) -> Result<CcxtCallOutcome, String> {
        let payload = serde_json::to_string(&request)
            .map_err(|error| format!("编码 CCXT Worker 请求失败: {error}"))?;
        let mut bytes = payload.into_bytes();
        bytes.push(b'\n');
        let stdin = self
            .stdin
            .take()
            .ok_or_else(|| "CCXT Worker stdin 不可用".to_string())?;
        let started = Instant::now();
        let timeout = Duration::from_millis(self.timeout_ms);
        let written =
            write_all_within_cancellable(stdin, bytes, timeout, &mut should_cancel, || {
                self.stop_child()
            })?;
        self.stdin = match written {
            CancellableWrite::Written(stdin) => Some(stdin),
            CancellableWrite::Cancelled => return Ok(CcxtCallOutcome::Cancelled),
        };

        loop {
            if should_cancel() {
                self.stop_child();
                return Ok(CcxtCallOutcome::Cancelled);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                self.stop_child();
                return Err(format!(
                    "CCXT Worker 响应超时 timeout_ms={}，提交结果未知",
                    self.timeout_ms
                ));
            }
            match self
                .responses
                .recv_timeout(remaining.min(Duration::from_millis(20)))
            {
                Ok(result) => {
                    let line = result?;
                    let response: Value = serde_json::from_str(&line)
                        .map_err(|error| format!("解析 CCXT Worker 响应失败: {error}"))?;
                    if response.get("ok").and_then(Value::as_bool) != Some(true) {
                        let error = response.get("error").cloned().unwrap_or_else(|| json!({}));
                        let class = error
                            .get("class")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        return Err(format!("CCXT Worker [{class}] {error}"));
                    }
                    return Ok(CcxtCallOutcome::Response(
                        response.get("result").cloned().unwrap_or(Value::Null),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("CCXT Worker 响应通道已断开，提交结果未知".into());
                }
            }
        }
    }

    fn stop_child(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stdin.take();
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

/// Worker 读窗的下界。用户流的空闲回话窗口由 `ccxt_idle_window_ms` 从它推导，
/// 而那条循环自身不睡 —— 下界低于一次 IPC 往返时，一个当天没有成交的账户会把
/// "刷新 EventLog + 写健康 mark"打成毫秒级热循环（V13 R2 第十四遍 #214）。
pub(crate) const CCXT_WORKER_MIN_TIMEOUT_MS: u64 = 1_000;
/// 读窗的上界：再长就等于提交结果未知要挂这么久才具名上报。
pub(crate) const CCXT_WORKER_MAX_TIMEOUT_MS: u64 = 300_000;

/// 用户流给子进程的回话窗口：读窗的 4/5，让"这一窗没有事件"的回话先于读窗到期
/// 到达，读窗到期因此只代表子进程真的卡住。定义挪到读窗的同一侧，是为了让
/// "下界 → 节律"这条不变式有用例可咬；原先它写在调用方的循环里，两侧各自漂移。
pub fn ccxt_idle_window_ms(timeout_ms: u64) -> u64 {
    timeout_ms.saturating_mul(4) / 5
}

fn ccxt_worker_timeout_ms(config_path: &str) -> Result<u64, String> {
    let config = ccxt_worker_config(config_path)?;
    let timeout_ms = config
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30_000);
    if !(CCXT_WORKER_MIN_TIMEOUT_MS..=CCXT_WORKER_MAX_TIMEOUT_MS).contains(&timeout_ms) {
        return Err(format!(
            "CCXT timeout_ms 必须在 {}..={} 内",
            CCXT_WORKER_MIN_TIMEOUT_MS, CCXT_WORKER_MAX_TIMEOUT_MS
        ));
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
        // 写入必须与读取同受 timeout_ms 约束：子进程一旦"活着但不再读 stdin"，超出 OS 匿名管道
        // 缓冲（~64KB）的那段 write_all 会永久阻塞，而 timeout_ms 只守着读侧 recv——本进程就此
        // 卡死，连关停令牌都读不到。三处同类缺陷共用同一条带预算的写入通道（qx-adapter::io_budget）。
        let mut bytes = payload.into_bytes();
        bytes.push(b'\n');
        let stdin = self
            .stdin
            .take()
            .ok_or_else(|| "CCXT Worker stdin 不可用".to_string())?;
        let written =
            write_all_within(stdin, bytes, Duration::from_millis(self.timeout_ms), || {
                let _ = self.child.kill();
                let _ = self.child.wait();
            });
        self.stdin = Some(written.map_err(|error| format!("写入 CCXT Worker 失败: {error}"))?);
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
                return Err("CCXT Worker 响应通道已断开，提交结果未知".into());
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

    fn call_cancellable(
        &mut self,
        request: Value,
        should_cancel: &mut dyn FnMut() -> bool,
    ) -> Result<CcxtCallOutcome, String> {
        CcxtProcessClient::call_cancellable(self, request, should_cancel)
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
    shutdown_check: Option<Box<dyn FnMut() -> bool + Send>>,
    connected: bool,
    /// 本地订单状态、远端订单号映射与成交去重键共用同一封顶策略：终态订单在超过
    /// `MAX_CACHED_ORDERS + ORDER_EVICT_HYSTERESIS` 后按 client_id 升序退场，派生索引同步级联。
    /// 退场即升级对账：被退订单再收到回报会落到「CCXT 本地订单不存在」出口，不会被静默收下。
    orders: BTreeMap<u64, Order>,
    remote_ids: BTreeMap<u64, String>,
    seen_trade_ids: BTreeMap<u64, BTreeSet<String>>,
    cumulative_costs: BTreeMap<u64, i128>,
    cumulative_fees: BTreeMap<u64, BTreeMap<String, i128>>,
    exact_cumulative_fees: BTreeMap<u64, BTreeMap<String, ExactDecimal>>,
}

impl OrderLifecycle for CcxtProcessVenue {}

fn validate_ccxt_hedge_direction(order: &Order, policy: OrderPolicy) -> QxResult<()> {
    if policy.position_mode != PositionMode::Hedge {
        if policy.position_side != PositionSide::Net {
            return Err(QxError::BusinessViolation(
                "CCXT OneWay 订单的 position_side 必须为 Net".into(),
            ));
        }
        return Ok(());
    }
    let is_close_side =
        match (policy.position_side, order.side) {
            (PositionSide::Long, qx_core::Side::Sell)
            | (PositionSide::Short, qx_core::Side::Buy) => true,
            (PositionSide::Long, qx_core::Side::Buy)
            | (PositionSide::Short, qx_core::Side::Sell) => false,
            (PositionSide::Net, _) => {
                return Err(QxError::BusinessViolation(
                    "CCXT Hedge 订单必须指定 Long 或 Short 仓位方向".into(),
                ));
            }
        };
    if policy.reduce_only != is_close_side {
        return Err(QxError::BusinessViolation(
            "CCXT Hedge 订单 side/reduce_only 与 position_side 不一致".into(),
        ));
    }
    Ok(())
}

impl CcxtProcessVenue {
    pub fn new(id: impl Into<String>, rpc: Box<dyn CcxtRpc>) -> Self {
        Self {
            id: id.into(),
            rpc,
            shutdown_check: None,
            connected: true,
            orders: BTreeMap::new(),
            remote_ids: BTreeMap::new(),
            seen_trade_ids: BTreeMap::new(),
            cumulative_costs: BTreeMap::new(),
            cumulative_fees: BTreeMap::new(),
            exact_cumulative_fees: BTreeMap::new(),
        }
    }

    /// Attach the owning worker's cancellation token to all subsequent venue RPC calls.
    pub fn with_shutdown_check(
        mut self,
        should_stop: impl FnMut() -> bool + Send + 'static,
    ) -> Self {
        self.shutdown_check = Some(Box::new(should_stop));
        self
    }

    /// 订单缓存封顶后级联清掉派生索引：远端订单号、成交去重键、累计成本都必须跟着
    /// 被退场的订单一起消失，否则缓存里照样留着账户全部历史，封顶就成了摆设。
    fn forget_orders(&mut self, evicted: &BTreeSet<u64>) {
        if evicted.is_empty() {
            return;
        }
        self.remote_ids
            .retain(|client_id, _| !evicted.contains(client_id));
        self.seen_trade_ids
            .retain(|client_id, _| !evicted.contains(client_id));
        self.cumulative_costs
            .retain(|client_id, _| !evicted.contains(client_id));
        self.cumulative_fees
            .retain(|client_id, _| !evicted.contains(client_id));
        self.exact_cumulative_fees
            .retain(|client_id, _| !evicted.contains(client_id));
    }

    /// 拉取订单事实并生成增量成交/取消事件。该方法可由 REST 轮询或 CCXT Pro
    /// 用户流 worker 调用，二者都复用同一事实转换。
    pub fn sync_order(&mut self, client_order_id: u64, ts: u64) -> QxResult<Vec<VenueEvent>> {
        self.capabilities().require_query()?;
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
        if order.exact_amounts.is_some() {
            return self.sync_exact_order(client_order_id, ts, &order, &remote_id, remote);
        }
        let filled = raw_i128(remote, "filled_raw")?;
        if filled < order.filled.raw() || filled > order.qty.raw() {
            return Err(QxError::ReconcileRequired(
                "CCXT filled 与本地订单不一致".into(),
            ));
        }
        let remote_status = remote
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_ascii_lowercase();
        match remote_status.as_str() {
            "rejected" if filled == 0 => {
                return Err(QxError::Permanent("CCXT 订单被交易所拒绝".into()));
            }
            "rejected" => {
                return Err(QxError::ReconcileRequired(
                    "CCXT 订单状态为 rejected 但已有成交，禁止静默归约".into(),
                ));
            }
            "open" | "new" | "live" | "pending" | "accepted" | "partially_filled"
            | "partially-filled"
                if filled < order.qty.raw() => {}
            "canceled" | "cancelled" | "expired" if filled <= order.qty.raw() => {}
            "closed" | "filled" | "done" if filled == order.qty.raw() => {}
            "closed" | "filled" | "done" => {
                return Err(QxError::ReconcileRequired(
                    "CCXT 订单已关闭但成交数量不足，无法判断剩余数量是否已撤销".into(),
                ));
            }
            "open" | "new" | "live" | "pending" | "accepted" | "partially_filled"
            | "partially-filled" => {
                return Err(QxError::ReconcileRequired(
                    "CCXT 订单状态仍为未完成但成交数量已达到订单数量".into(),
                ));
            }
            _ => {
                return Err(QxError::ReconcileRequired(format!(
                    "CCXT 返回未知订单状态: {remote_status}"
                )));
            }
        }
        let mut events = Vec::new();
        let prior_filled = order.filled.raw();
        let mut prior_cost = self
            .cumulative_costs
            .get(&client_order_id)
            .copied()
            .unwrap_or(0);
        if filled > prior_filled
            && prior_filled > 0
            && !self.cumulative_costs.contains_key(&client_order_id)
        {
            // A process can restart after a partial fill but before the next reconciliation.
            // Rebuild the old cumulative cost from the complete, order-filtered trade history;
            // if that history cannot prove an exact quantity boundary, keep the order fail-closed.
            prior_cost = self.recover_cumulative_cost_from_trades(
                &order.instrument.to_string(),
                &remote_id,
                prior_filled,
                filled,
            )?;
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
        let fee_raw = remote
            .get("fee_raw")
            .map(|value| raw_i128_value(value, "fee_raw"))
            .transpose()?
            .unwrap_or(0);
        let fee_currency = remote
            .get("fee_currency")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut cumulative_fee_rows = parse_fee_charges(remote)?;
        if cumulative_fee_rows.is_empty() && fee_raw != 0 {
            let currency = fee_currency
                .as_deref()
                .map(str::trim)
                .filter(|currency| !currency.is_empty())
                .ok_or_else(|| {
                    QxError::ReconcileRequired(
                        "CCXT 返回非零成交费用但缺少币种，拒绝猜测费用单位".into(),
                    )
                })?
                .to_ascii_uppercase();
            cumulative_fee_rows.push(FillFee {
                amount: Money::from_raw(fee_raw),
                currency: Some(currency),
                source_trade_ids: Vec::new(),
            });
        }
        let mut next_fee_baseline = self
            .cumulative_fees
            .get(&client_order_id)
            .cloned()
            .unwrap_or_default();
        let (fee_deltas, fee_trade_ids) = if !cumulative_fee_rows.is_empty() {
            (
                Self::cumulative_fee_delta(&mut next_fee_baseline, cumulative_fee_rows)?,
                Vec::new(),
            )
        } else {
            let (incremental, trade_ids) = self.incremental_trade_fee(
                client_order_id,
                &order.instrument.to_string(),
                &remote_id,
            )?;
            Self::add_incremental_fees(&mut next_fee_baseline, &incremental)?;
            (incremental, trade_ids)
        };
        if filled > order.filled.raw() {
            if order.status.is_terminal() {
                // 本地已终态（典型场景是撤单落地后远端又推进了成交）：这是本地与远端的
                // 真实分歧，只能交给对账，不能伪造一条增量成交让上层记账或改写终态。
                return Err(QxError::ReconcileRequired(format!(
                    "CCXT 订单 {client_order_id} 本地已终态 {:?}，但远端累计成交 {} 大于本地 {}",
                    order.status,
                    filled,
                    order.filled.raw()
                )));
            }
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
            let (fill_fee_raw, fill_fee_currency) = if fee_deltas.len() == 1 {
                (fee_deltas[0].amount.raw(), fee_deltas[0].currency.clone())
            } else {
                (0, None)
            };
            let mut fill = Fill {
                order_id: client_order_id,
                qty: Quantity::from_raw(delta_qty),
                price: Price::from_raw(price),
                fee: Money::from_raw(fill_fee_raw),
                ts,
                account_id: order.account_id.clone(),
                strategy_id: None,
                signal_id: None,
                intent_id: None,
                venue_id: Some(self.id.clone()),
                venue_order_id: Some(remote_id.clone()),
                rule_version: None,
                fee_currency: fill_fee_currency,
                fees: fee_deltas.clone(),
                exact_fees: vec![],
                exact_amounts: None,
            };
            order.trace_fill(&mut fill, Some(&self.id), Some(&remote_id));
            events.push(VenueEvent::Fill(fill));
        } else if !fee_deltas.is_empty() {
            events.push(VenueEvent::FeeAdjustment {
                account_id: order.account_id.clone(),
                venue_id: self.id.clone(),
                order_id: client_order_id,
                instrument: order.instrument.clone(),
                fees: fee_deltas,
                ts,
            });
        }
        self.cumulative_costs
            .insert(client_order_id, cumulative_cost);
        self.cumulative_fees
            .insert(client_order_id, next_fee_baseline);
        let seen = self.seen_trade_ids.entry(client_order_id).or_default();
        for trade_id in fee_trade_ids {
            Self::remember_fill(seen, trade_id);
        }
        match remote_status.as_str() {
            "canceled" | "cancelled" | "expired"
                if !order.status.is_terminal() && filled < order.qty.raw() =>
            {
                events.push(VenueEvent::Cancelled {
                    client_order_id,
                    ts,
                })
            }
            _ => {}
        }
        if let Some(local) = self.orders.get_mut(&client_order_id) {
            local.filled = Quantity::from_raw(filled);
            if filled == local.qty.raw() {
                local.status = OrderStatus::Filled;
            } else if matches!(remote_status.as_str(), "canceled" | "cancelled" | "expired") {
                local.status = OrderStatus::Cancelled;
            } else if filled > 0 && local.status != OrderStatus::CancelPending {
                local.status = OrderStatus::PartiallyFilled;
            }
        }
        Ok(events)
    }

    fn call_or_reconcile(&mut self, request: Value) -> QxResult<Value> {
        let response = match self.shutdown_check.as_mut() {
            Some(should_stop) => self
                .rpc
                .call_cancellable(request, should_stop.as_mut())
                .and_then(|outcome| match outcome {
                    CcxtCallOutcome::Response(value) => Ok(value),
                    CcxtCallOutcome::Cancelled => Err("CCXT Worker 请求因停机取消".into()),
                }),
            None => self.rpc.call(request),
        };
        match response {
            Ok(result) => Ok(result),
            Err(error) => {
                // 一旦 RPC 返回错误，不能继续把本地 Venue 当作可提交状态；
                // 调用方必须先 replace_rpc 或交给对账流程恢复。
                let error = QxError::ReconcileRequired(format!("CCXT Worker 结果未知: {error}"));
                if Self::reconciliation_trigger(&error) {
                    self.connected = false;
                }
                Err(error)
            }
        }
    }

    /// 将 CCXT Pro/扩展 JSONL 事件请求保持在同一进程边界；调用方只能得到
    /// 原始观察结果，订单事实仍必须通过 `sync_order` 和 Runtime 归约。
    pub fn stream_call(&mut self, request: Value) -> QxResult<Value> {
        self.call_or_reconcile(request)
    }

    pub fn stream_call_cancellable(
        &mut self,
        request: Value,
        should_cancel: &mut dyn FnMut() -> bool,
    ) -> QxResult<CcxtCallOutcome> {
        match self.rpc.call_cancellable(request, should_cancel) {
            Ok(CcxtCallOutcome::Cancelled) => {
                self.connected = false;
                Ok(CcxtCallOutcome::Cancelled)
            }
            Ok(response) => Ok(response),
            Err(error) => {
                let error = QxError::ReconcileRequired(format!("CCXT Worker 结果未知: {error}"));
                if Self::reconciliation_trigger(&error) {
                    self.connected = false;
                }
                Err(error)
            }
        }
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
    ) -> QxResult<(Vec<FillFee>, Vec<String>)> {
        let result = match self.rpc.call(json!({
            "op": "fetch_my_trades",
            "instrument": instrument,
            "order_id": remote_order_id,
        })) {
            Ok(result) => result,
            Err(error) if error.contains("[unsupported]") => return Ok((Vec::new(), Vec::new())),
            Err(error) => {
                self.connected = false;
                return Err(QxError::ReconcileRequired(format!(
                    "CCXT 成交费用查询结果未知: {error}"
                )));
            }
        };
        let trades = result
            .get("trades")
            .and_then(Value::as_array)
            .ok_or_else(|| QxError::ReconcileRequired("CCXT trades 响应缺少 trades".into()))?;
        let seen = self.seen_trade_ids.get(&client_order_id);
        let mut fees_by_currency = BTreeMap::<String, (i128, BTreeSet<String>)>::new();
        let mut observed_trade_ids = BTreeSet::new();
        let mut pending_trade_ids = BTreeSet::new();
        for trade in trades {
            let Some(trade_id) = trade
                .get("trade_id")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                return Err(QxError::ReconcileRequired(
                    "CCXT trade 缺少 trade_id".into(),
                ));
            };
            if seen.is_some_and(|seen| seen.contains(&trade_id))
                || !observed_trade_ids.insert(trade_id.clone())
            {
                continue;
            }
            let mut trade_fees = parse_fee_charges(trade)?;
            if trade_fees.is_empty() {
                let amount = raw_i128(trade, "fee_raw")?;
                let currency = trade
                    .get("fee_currency")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_ascii_uppercase);
                if amount != 0 {
                    let currency = currency.ok_or_else(|| {
                        QxError::ReconcileRequired(
                            "CCXT 成交费用缺少币种，拒绝按结算币种猜测".into(),
                        )
                    })?;
                    trade_fees.push(FillFee {
                        amount: Money::from_raw(amount),
                        currency: Some(currency),
                        source_trade_ids: Vec::new(),
                    });
                }
            }
            let has_nonzero_fee = trade_fees.iter().any(|charge| charge.amount.raw() != 0);
            for charge in trade_fees {
                let currency = charge
                    .currency
                    .as_deref()
                    .map(str::trim)
                    .filter(|currency| !currency.is_empty())
                    .ok_or_else(|| {
                        QxError::ReconcileRequired(
                            "CCXT 成交费用缺少币种，拒绝按结算币种猜测".into(),
                        )
                    })?
                    .to_ascii_uppercase();
                if charge.amount.raw() == 0 {
                    continue;
                }
                let (total, source_trade_ids) = fees_by_currency.entry(currency).or_default();
                *total = total
                    .checked_add(charge.amount.raw())
                    .ok_or_else(|| QxError::Invariant("CCXT 成交费用累计溢出".into()))?;
                source_trade_ids.insert(trade_id.clone());
            }
            // Do not permanently consume a trade that currently reports no fee. Some venues
            // attach commission details after the initial fill event; a later trade-fee query
            // (for example on the next incremental fill) must be able to observe that charge.
            if has_nonzero_fee {
                pending_trade_ids.insert(trade_id);
            }
        }
        // Validate and aggregate the complete result before advancing the dedup ledger. If a
        // malformed/mixed-currency trade is discovered midway, a later poll must see every
        // trade again; otherwise the retry could emit a fill with the fee silently reduced to
        // zero after the invalid row was already marked as consumed.
        if let Some(prior_totals) = self.cumulative_fees.get(&client_order_id) {
            for (currency, (amount, _)) in &fees_by_currency {
                prior_totals
                    .get(currency)
                    .copied()
                    .unwrap_or(0)
                    .checked_add(*amount)
                    .ok_or_else(|| QxError::Invariant("CCXT 累计手续费溢出".into()))?;
            }
        }
        Ok((
            fees_by_currency
                .into_iter()
                .filter(|(_, (amount, _))| *amount != 0)
                .map(|(currency, (amount, source_trade_ids))| FillFee {
                    amount: Money::from_raw(amount),
                    currency: Some(currency),
                    source_trade_ids: source_trade_ids.into_iter().collect(),
                })
                .collect(),
            pending_trade_ids.into_iter().collect(),
        ))
    }

    fn recover_cumulative_cost_from_trades(
        &mut self,
        instrument: &str,
        remote_order_id: &str,
        prior_filled_raw: i128,
        current_filled_raw: i128,
    ) -> QxResult<i128> {
        let result = match self.rpc.call(json!({
            "op": "fetch_my_trades",
            "instrument": instrument,
            "order_id": remote_order_id,
        })) {
            Ok(result) => result,
            Err(error) if error.contains("[unsupported]") => {
                return Err(QxError::ReconcileRequired(
                    "CCXT 重启后部分成交订单缺少累计成本，且交易所不支持按订单读取逐笔成交；转人工对账".into(),
                ));
            }
            Err(error) => {
                self.connected = false;
                return Err(QxError::ReconcileRequired(format!(
                    "CCXT 恢复部分成交成本时逐笔成交查询结果未知: {error}"
                )));
            }
        };
        let trades = result
            .get("trades")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                QxError::ReconcileRequired("CCXT 恢复部分成交成本时响应缺少 trades 数组".into())
            })?;
        if trades.is_empty() {
            return Err(QxError::ReconcileRequired(
                "CCXT 恢复部分成交成本时没有订单逐笔成交记录".into(),
            ));
        }

        let mut rows = Vec::with_capacity(trades.len());
        for trade in trades {
            let trade_id = trade
                .get("trade_id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    QxError::ReconcileRequired(
                        "CCXT 恢复部分成交成本时逐笔记录缺少 trade_id".into(),
                    )
                })?
                .to_string();
            let timestamp_ms = trade
                .get("timestamp_ms")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    QxError::ReconcileRequired(format!(
                        "CCXT trade {trade_id} 缺少成交时间，无法恢复累计成本顺序"
                    ))
                })?;
            let amount_raw = raw_i128(trade, "amount_raw")?;
            let price_raw = raw_i128(trade, "price_raw")?;
            if amount_raw <= 0 || price_raw <= 0 {
                return Err(QxError::ReconcileRequired(format!(
                    "CCXT trade {trade_id} 的数量或价格非法，无法恢复累计成本"
                )));
            }
            let cost_raw = amount_raw
                .checked_mul(price_raw)
                .and_then(|value| value.checked_div(SCALE))
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    QxError::ReconcileRequired(format!(
                        "CCXT trade {trade_id} 的成交成本无法精确表示"
                    ))
                })?;
            rows.push((timestamp_ms, trade_id, amount_raw, cost_raw));
        }
        rows.sort_by(|left, right| {
            left.0.cmp(&right.0).then_with(|| {
                match (left.1.parse::<u128>().ok(), right.1.parse::<u128>().ok()) {
                    (Some(left), Some(right)) => left.cmp(&right),
                    _ => left.1.cmp(&right.1),
                }
            })
        });

        let mut total_qty = 0_i128;
        let mut total_cost = 0_i128;
        let mut prior_cost = None;
        for (_, trade_id, amount_raw, cost_raw) in rows {
            if prior_cost.is_none() {
                if total_qty == prior_filled_raw {
                    prior_cost = Some(total_cost);
                } else if total_qty < prior_filled_raw
                    && total_qty
                        .checked_add(amount_raw)
                        .is_some_and(|next| next > prior_filled_raw)
                {
                    return Err(QxError::ReconcileRequired(format!(
                        "CCXT 本地已记成交量 {} 落在 trade {trade_id} 的逐笔数量内部，不能精确恢复成本",
                        prior_filled_raw
                    )));
                }
            }
            total_qty = total_qty
                .checked_add(amount_raw)
                .ok_or_else(|| QxError::Invariant("CCXT trade 成交量累计溢出".into()))?;
            total_cost = total_cost
                .checked_add(cost_raw)
                .ok_or_else(|| QxError::Invariant("CCXT trade 成交成本累计溢出".into()))?;
        }
        if total_qty != current_filled_raw {
            return Err(QxError::ReconcileRequired(format!(
                "CCXT 逐笔成交未覆盖当前订单数量：trade_qty={total_qty} remote_filled={current_filled_raw}"
            )));
        }
        prior_cost
            .or_else(|| (total_qty == prior_filled_raw).then_some(total_cost))
            .ok_or_else(|| {
                QxError::ReconcileRequired(format!(
                    "CCXT 逐笔成交历史未能覆盖本地已记成交量 {prior_filled_raw}"
                ))
            })
    }

    /// Order fee fields are cumulative. Emit only the per-currency delta for this incremental
    /// fill while remembering the latest cumulative total for the next poll.
    fn cumulative_fee_delta(
        prior: &mut BTreeMap<String, i128>,
        cumulative: Vec<FillFee>,
    ) -> QxResult<Vec<FillFee>> {
        let mut deltas = Vec::new();
        for charge in cumulative {
            let currency = charge
                .currency
                .as_deref()
                .map(str::trim)
                .filter(|currency| !currency.is_empty())
                .ok_or_else(|| QxError::ReconcileRequired("CCXT 累计手续费缺少币种".into()))?
                .to_ascii_uppercase();
            let previous = prior.get(&currency).copied().unwrap_or(0);
            let delta = charge
                .amount
                .raw()
                .checked_sub(previous)
                .ok_or_else(|| QxError::Invariant("CCXT 手续费增量溢出".into()))?;
            prior.insert(currency.clone(), charge.amount.raw());
            if delta != 0 {
                deltas.push(FillFee {
                    amount: Money::from_raw(delta),
                    currency: Some(currency),
                    source_trade_ids: Vec::new(),
                });
            }
        }
        Ok(deltas)
    }

    /// Trade fees are already incremental; fold them into the cumulative baseline so a later
    /// order snapshot that starts reporting total fees cannot charge those trades twice.
    fn add_incremental_fees(
        totals: &mut BTreeMap<String, i128>,
        incremental: &[FillFee],
    ) -> QxResult<()> {
        for charge in incremental {
            let currency = charge
                .currency
                .as_deref()
                .map(str::trim)
                .filter(|currency| !currency.is_empty())
                .ok_or_else(|| QxError::ReconcileRequired("CCXT 成交手续费缺少币种".into()))?
                .to_ascii_uppercase();
            let total = totals.entry(currency).or_default();
            *total = total
                .checked_add(charge.amount.raw())
                .ok_or_else(|| QxError::Invariant("CCXT 累计手续费溢出".into()))?;
        }
        Ok(())
    }
}

impl Venue for CcxtProcessVenue {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self) -> &'static qx_zhenlu::venue_capabilities::VenueCapabilities {
        &qx_zhenlu::venue_capabilities::CCXT_PROCESS_VENUE_CAPABILITIES
    }

    fn validate_instrument_spec(&mut self, spec: &qx_core::TradingInstrumentSpec) -> QxResult<()> {
        let product = match spec.product {
            qx_core::TradingProduct::Spot => "spot",
            qx_core::TradingProduct::Margin => "margin",
            qx_core::TradingProduct::Perpetual => "perpetual",
            qx_core::TradingProduct::Future => "future",
            qx_core::TradingProduct::Option => "option",
        };
        let exact_rules = spec.exact_rules.as_ref().map(|rules| {
            json!({
                "contract_size": rules.contract_size,
                "price_tick": rules.price_tick,
                "quantity_step": rules.quantity_step,
                "minimum_quantity": rules.minimum_quantity,
                "minimum_notional": rules.minimum_notional,
            })
        });
        let response = self.stream_call(json!({
            "op": "validate_market_spec",
            "instrument": spec.instrument.to_string(),
            "expected_spec": {
                "product": product,
                "base_currency": spec.base_currency.as_str(),
                "quote_currency": spec.quote_currency.as_str(),
                "settlement_currency": spec.settlement_currency.as_str(),
                "contract_size_raw": spec.contract_size.to_string(),
                "linear": spec.linear,
                "inverse": spec.inverse,
                "price_tick_raw": spec.price_tick.to_string(),
                "qty_step_raw": spec.qty_step.to_string(),
                "min_qty_raw": spec.min_qty.to_string(),
                "min_notional_raw": spec.min_notional_raw.to_string(),
                "max_leverage": spec.max_leverage,
                "maintenance_margin_bps": spec.maintenance_margin_bps,
                "exact_rules": exact_rules,
            }
        }))?;
        if let Some(rejection) = response.get("market_spec_rejected") {
            let class = rejection
                .get("class")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let message = rejection
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("交易所市场规格与冻结风控规格不一致");
            let message = format!("CCXT 当前市场规格校验 [{class}] {message}");
            return Err(match class {
                "invalid" | "unsupported" => QxError::BusinessViolation(message),
                "retryable" => QxError::Transient(message),
                "rate_limited" => QxError::ResourceExhausted(message),
                _ => QxError::Permanent(message),
            });
        }
        if response
            .get("market_spec_verified")
            .and_then(Value::as_bool)
            == Some(true)
        {
            Ok(())
        } else {
            Err(QxError::Permanent(
                "CCXT Worker 未确认当前市场规格与冻结规格一致".into(),
            ))
        }
    }

    fn submit(&mut self, order: Order, ts: u64) -> QxResult<Vec<VenueEvent>> {
        self.capabilities()
            .validate_submit(&order, self.id())
            .map_err(QxError::BusinessViolation)?;
        Self::validate_order_spec(&order)?;
        if !Self::can_submit(&order) {
            return Err(QxError::BusinessViolation(
                "CCXT 只接受待提交或已提交订单".into(),
            ));
        }
        if !self.connected {
            return Err(QxError::VenueState("CCXT Worker 未连接".into()));
        }
        let order_type = if order.limit_price_decimal().is_some() {
            "limit"
        } else {
            "market"
        };
        let mut params = serde_json::Map::new();
        let policy = order.policy.unwrap_or_default();
        validate_ccxt_hedge_direction(&order, policy)?;
        if policy.leverage == 0 {
            return Err(QxError::BusinessViolation("CCXT 订单杠杆必须大于 0".into()));
        }
        if policy.margin_mode == MarginMode::Cash && policy.leverage != 1 {
            return Err(QxError::BusinessViolation(
                "CCXT Cash 订单只能使用 1x 杠杆".into(),
            ));
        }

        // Mode configuration is account-scoped or product-scoped on the exchanges. Never
        // toggle it as an order side effect: verify the operator-configured account state before
        // any write, and fail closed if policy and exchange disagree.
        let settings = self.call_or_reconcile(json!({
            "op": "fetch_contract_settings",
            "instrument": order.instrument.to_string(),
        }))?;
        let is_contract = settings
            .pointer("/settings/contract")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                QxError::ReconcileRequired("CCXT 合约设置查询缺少 contract 标记，拒绝提交".into())
            })?;
        if is_contract {
            if policy.margin_mode == MarginMode::Cash {
                return Err(QxError::BusinessViolation(
                    "合约订单必须显式选择 Cross 或 Isolated 保证金模式".into(),
                ));
            }
            let expected_position_mode = if policy.position_mode == PositionMode::Hedge {
                "hedge"
            } else {
                "one_way"
            };
            let actual_position_mode = settings
                .pointer("/settings/position_mode")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    QxError::ReconcileRequired(
                        "CCXT 合约设置查询缺少 position_mode，拒绝提交".into(),
                    )
                })?;
            if actual_position_mode != expected_position_mode {
                return Err(QxError::BusinessViolation(format!(
                    "交易所持仓模式为 {actual_position_mode}，订单要求 {expected_position_mode}；请先在账户侧配置一致"
                )));
            }
            let expected_margin_mode = match policy.margin_mode {
                MarginMode::Cross => "cross",
                MarginMode::Isolated => "isolated",
                MarginMode::Cash => unreachable!("cash contract policy rejected above"),
            };
            let actual_margin_mode = settings
                .pointer("/settings/margin_mode")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    QxError::ReconcileRequired("CCXT 合约设置查询缺少 margin_mode，拒绝提交".into())
                })?;
            if actual_margin_mode != expected_margin_mode {
                return Err(QxError::BusinessViolation(format!(
                    "交易所保证金模式为 {actual_margin_mode}，订单要求 {expected_margin_mode}；请先在账户侧配置一致"
                )));
            }
            // OKX requires tdMode on every contract order, and Bitget Classic's CCXT mapping
            // uses marginMode to select its v2 contract request. Bitget UTA v3 configures
            // margin mode at account/position scope and does not accept the Classic field, so
            // the worker must report which Bitget account path it verified.
            let exchange_id = self.id.to_ascii_lowercase();
            let bitget_uta = if exchange_id == "bitget" {
                Some(
                    settings
                        .pointer("/settings/uta")
                        .and_then(Value::as_bool)
                        .ok_or_else(|| {
                            QxError::ReconcileRequired(
                                "Bitget 合约设置缺少 UTA/Classic 路径标记，拒绝提交".into(),
                            )
                        })?,
                )
            } else {
                None
            };
            if exchange_id == "okx" || (exchange_id == "bitget" && bitget_uta == Some(false)) {
                params.insert(
                    "marginMode".into(),
                    Value::String(expected_margin_mode.to_string()),
                );
            }
            let leverage_long = settings
                .pointer("/settings/leverage_long")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| {
                    QxError::ReconcileRequired(
                        "CCXT 合约设置查询缺少有效 long leverage，拒绝提交".into(),
                    )
                })?;
            let leverage_short = settings
                .pointer("/settings/leverage_short")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| {
                    QxError::ReconcileRequired(
                        "CCXT 合约设置查询缺少有效 short leverage，拒绝提交".into(),
                    )
                })?;
            let leverage_matches = match (policy.position_mode, policy.position_side) {
                (PositionMode::Hedge, PositionSide::Long) => leverage_long == policy.leverage,
                (PositionMode::Hedge, PositionSide::Short) => leverage_short == policy.leverage,
                (PositionMode::Hedge, PositionSide::Net) => false,
                (PositionMode::OneWay, PositionSide::Net) => {
                    leverage_long == policy.leverage && leverage_short == policy.leverage
                }
                (PositionMode::OneWay, PositionSide::Long | PositionSide::Short) => false,
            };
            if !leverage_matches {
                return Err(QxError::BusinessViolation(format!(
                    "交易所杠杆 long={leverage_long}x/short={leverage_short}x 与订单目标 {}x 不一致；请先在账户侧配置一致",
                    policy.leverage
                )));
            }
        } else if policy.position_mode == PositionMode::Hedge
            || policy.margin_mode != MarginMode::Cash
            || policy.leverage != 1
        {
            return Err(QxError::BusinessViolation(
                "现货订单不支持 Hedge、保证金模式或大于 1x 杠杆".into(),
            ));
        }

        if order.policy.is_some() {
            // Keep reduceOnly as intent for CCXT to translate. In hedge mode CCXT uses the
            // combination of `hedged` and `reduceOnly` to choose the correct Binance
            // positionSide / OKX posSide / Bitget close leg, then omits or rewrites the field
            // where the venue does not accept it.
            if policy.reduce_only {
                params.insert("reduceOnly".into(), Value::Bool(true));
            }
            if policy.post_only {
                params.insert("postOnly".into(), Value::Bool(true));
            }
            // Position fields are venue-specific. CCXT translates `hedged=true` into
            // Binance positionSide, OKX posSide, or Bitget tradeSide/posSide and maps reduce-only
            // closes correctly for each exchange.
            if policy.position_mode == PositionMode::Hedge {
                params.insert("hedged".into(), Value::Bool(true));
            }
        }
        let result = self.call_or_reconcile(json!({
            "op": "create_order",
            "instrument": order.instrument.to_string(),
            "side": if matches!(order.side, qx_core::Side::Buy) { "buy" } else { "sell" },
            "order_type": order_type,
            "amount_raw": order.exact_amounts.is_none().then_some(order.qty.raw()),
            "amount_exact": order.exact_amounts.map(|amounts| {
                amounts.quote_budget.unwrap_or(amounts.quantity)
            }),
            "amount_unit": if order.is_quote_budget_order() {
                "quote"
            } else if is_contract {
                "contracts"
            } else {
                "base"
            },
            "price_raw": order
                .exact_amounts
                .is_none()
                .then(|| order.limit.map(|price| price.raw()))
                .flatten(),
            "price_exact": order.exact_amounts.and_then(|amounts| amounts.limit_price),
            "risk_leverage": if is_contract { Some(policy.leverage) } else { None },
            "risk_reduce_only": policy.reduce_only,
            "risk_margin_mode": if is_contract {
                Some(match policy.margin_mode {
                    MarginMode::Cross => "cross",
                    MarginMode::Isolated => "isolated",
                    MarginMode::Cash => unreachable!("cash contract policy rejected above"),
                })
            } else {
                None::<&str>
            },
            "client_order_id": order.client_id.to_string(),
            "params": params,
        }))?;
        if let Some(preflight) = result.get("preflight_rejected") {
            let class = preflight
                .get("class")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let message = preflight
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("CCXT 合约杠杆档位预检拒绝订单");
            let message = format!("CCXT 下单前杠杆档位预检 [{class}] {message}");
            return Err(match class {
                "invalid" | "unsupported" => QxError::BusinessViolation(message),
                "retryable" => QxError::Transient(message),
                "rate_limited" => QxError::ResourceExhausted(message),
                _ => QxError::Permanent(message),
            });
        }
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
        self.cumulative_fees.remove(&order.client_id);
        self.exact_cumulative_fees.remove(&order.client_id);
        self.orders.insert(order.client_id, order.clone());
        let evicted = evict_stale_terminal_orders(&mut self.orders);
        self.forget_orders(&evicted);
        Ok(vec![VenueEvent::Accepted {
            client_order_id: order.client_id,
            venue_order_id: remote_id,
            ts,
        }])
    }

    fn cancel(&mut self, client_order_id: u64, ts: u64) -> QxResult<Vec<VenueEvent>> {
        self.capabilities()
            .validate_cancel()
            .map_err(QxError::BusinessViolation)?;
        let order = self
            .orders
            .get(&client_order_id)
            .cloned()
            .ok_or_else(|| QxError::Permanent("CCXT 本地订单不存在".into()))?;
        if order.status.is_terminal() {
            // 终态不再产生重复撤单事实。
            return Ok(Vec::new());
        }
        Self::validate_cancel_state(&order, "CCXT")?;
        let remote_id = self
            .remote_ids
            .get(&client_order_id)
            .cloned()
            .ok_or_else(|| QxError::ReconcileRequired("CCXT 订单缺少 remote order id".into()))?;
        self.call_or_reconcile(json!({
            "op": "cancel_order",
            "order_id": remote_id,
            "instrument": order.instrument.to_string(),
        }))?;
        // A successful cancel RPC is only an acknowledgement: the order may have filled
        // concurrently, may still be working, or may be in a venue-specific cancel-pending
        // state. Re-fetch the authoritative order and reuse the normal incremental fill and
        // terminal-state reconciliation path. Never manufacture a local Cancelled fact from
        // the cancel request response alone.
        self.sync_order(client_order_id, ts)
    }

    fn snapshot(&self) -> Vec<VenueOrderSnapshot> {
        self.orders
            .values()
            .map(|order| VenueOrderSnapshot {
                client_order_id: order.client_id,
                status: order.status,
                filled: order.filled,
                filled_exact: order.exact_amounts.map(|amounts| amounts.filled),
            })
            .collect()
    }

    fn connected(&self) -> bool {
        self.connected
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

fn parse_fee_charges(value: &Value) -> QxResult<Vec<FillFee>> {
    let Some(rows) = value.get("fees") else {
        return Ok(Vec::new());
    };
    let rows = rows
        .as_array()
        .ok_or_else(|| QxError::ReconcileRequired("CCXT fees 必须是逐币种费用数组".into()))?;
    let mut totals = BTreeMap::<String, i128>::new();
    for row in rows {
        let amount = raw_i128(row, "amount_raw")?;
        if amount == 0 {
            continue;
        }
        let currency = row
            .get("currency")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|currency| !currency.is_empty())
            .ok_or_else(|| QxError::ReconcileRequired("CCXT 非零手续费明细缺少币种".into()))?
            .to_ascii_uppercase();
        let total = totals.entry(currency).or_default();
        *total = total
            .checked_add(amount)
            .ok_or_else(|| QxError::Invariant("CCXT 手续费按币种累计溢出".into()))?;
    }
    Ok(totals
        .into_iter()
        .filter(|(_, amount)| *amount != 0)
        .map(|(currency, amount)| FillFee {
            amount: Money::from_raw(amount),
            currency: Some(currency),
            source_trade_ids: Vec::new(),
        })
        .collect())
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
#[path = "ccxt/tests.rs"]
mod tests;
