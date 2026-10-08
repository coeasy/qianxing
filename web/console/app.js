// 牵星 Qianxing · 控制台（M1' 快照 + M2' 实时事件 + M3' 控制面）
//
// 读面：读取 qx-api 的只读端点，并用 WebSocket 接收增量事件。
// 写面：**只有一条**——`POST /control/commands`（控制面受理入口）。它不发送订单：
// 下单要走这条命令，由控制面受理 → 判定执行者 → 走到终态退场，页面不直连任何下单端点。
//
// 下面这份 API_PATHS 是控制台与后端的唯一接线表：门禁 web_console_check 会
// 逐条核对这里的每个路径都真的出现在 qx-api 的路由表里，"前端调用了一个后端
// 不存在的端点"会在门禁当场变红，而不是等到用户点开页面才 404。

const API_PATHS = {
  health: "/health",
  ready: "/ready",
  metrics: "/metrics",
  snapshot: "/account/snapshot",
  snapshotEnvelope: "/account/snapshot/envelope",
  balances: "/account/balances",
  orders: "/account/orders",
  positions: "/account/positions",
  ledger: "/account/ledger",
  reconcileReports: "/reconcile/reports",
  schedulerRuns: "/scheduler/runs",
  controlAudit: "/control/audit",
  controlCommands: "/control/commands",
  events: "/events",
  eventsLive: "/events/live",
};

// M3' 的三阶段。它们是**页面对控制面语义的复述**，不是服务端新增的状态：
// 受理 = 控制面收下并落审计（202 + AuditRecord）；执行者判定 = 这条命令在本构建里
// 有没有执行者（`CommandKind::executed` 那一侧），没执行者的类型只会在审计里停在
// Accepted；终态退场 = 状态走到 Executed / Failed（`CommandStatus::is_final`）。
const PHASES = ["受理", "执行者判定", "终态退场"];
const FINAL_STATUSES = ["Executed", "Failed"];


// WebSocket 升级不占路由表：任何路径带 Upgrade: websocket 即在 HTTP 分派前转交
// ws.rs。这里借用 /events/live 作为通道名，路径本身不参与服务端判定。
const WS_PATH = "/events/live";

const POLL_MS = 5000;

const state = {
  base: "",
  timer: null,
  ws: null,
  wsAlive: false,
  cursor: 0,
  events: [],
  connected: false,
  commandSeq: 1,
  lastCommand: null,
};

const $ = (id) => document.getElementById(id);

function el(tag, text, cls) {
  const node = document.createElement(tag);
  if (text !== undefined && text !== null) node.textContent = String(text);
  if (cls) node.className = cls;
  return node;
}

// 未算出的钱是 null，不是 0：这里如实显示为 —，不折成 0（与后端 null ≠ 0 同口径）。
function money(value) {
  if (value === null || value === undefined) return "—";
  if (typeof value === "number") return value.toLocaleString("zh-CN");
  return String(value);
}

function rows(container, pairs) {
  container.replaceChildren();
  for (const [k, v] of pairs) {
    const row = el("div", null, "row");
    row.append(el("span", k, "k"), el("span", v, "v"));
    container.append(row);
  }
}

function normalizeBase(raw) {
  const trimmed = (raw || "").trim().replace(/\/+$/, "");
  if (!trimmed) return "";
  return /^https?:\/\//.test(trimmed) ? trimmed : `http://${trimmed}`;
}

function wsUrl(base) {
  return base.replace(/^http/, "ws") + WS_PATH;
}

async function getJson(path) {
  const response = await fetch(state.base + path, { headers: { Accept: "application/json" } });
  let body = null;
  try {
    body = await response.json();
  } catch (_) {
    body = null;
  }
  return { status: response.status, ok: response.ok, body };
}

function setConnState(text, cls) {
  const pill = $("conn-state");
  pill.textContent = text;
  pill.className = `pill ${cls}`;
}

// ---- 各面板刷新 ----

async function refreshHealth() {
  const [health, ready] = await Promise.all([getJson(API_PATHS.health), getJson(API_PATHS.ready)]);
  rows($("health-rows"), [
    ["/health", health.ok ? `200 ${health.body && health.body.status ? health.body.status : "ok"}` : `HTTP ${health.status}`],
    ["/ready", ready.ok ? `200 ready=${ready.body && ready.body.ready}` : `HTTP ${ready.status}${ready.body && ready.body.detail ? " " + ready.body.detail : ""}`],
  ]);
}

async function refreshSnapshot() {
  const snap = await getJson(API_PATHS.snapshot);
  if (snap.status === 404) {
    rows($("snapshot-rows"), [["状态", "404 无快照（该账户还没有投影）"]]);
    return;
  }
  const b = snap.body || {};
  rows($("snapshot-rows"), [
    ["schema_version", b.schema_version],
    ["state_hash", b.state_hash],
    ["as_of", b.as_of],
    ["account_id", b.account_id],
    ["venue_id", b.venue_id],
  ]);
}

async function refreshBalances() {
  const bal = await getJson(API_PATHS.balances);
  const b = (bal.ok && bal.body) || {};
  const cash = b.cash_raw && typeof b.cash_raw === "object"
    ? Object.entries(b.cash_raw).map(([k, v]) => `${k}=${money(v)}`).join("  ") || "（空）"
    : money(b.cash_raw);
  rows($("balances-rows"), [
    ["cash_raw", cash],
    ["equity_raw", money(b.equity_raw)],
    ["available_raw", money(b.available_raw)],
    ["margin_raw", money(b.margin_raw)],
  ]);
}

function fillTable(tableId, records, cells) {
  const tbody = $(tableId).querySelector("tbody");
  tbody.replaceChildren();
  if (!Array.isArray(records) || records.length === 0) {
    const tr = el("tr");
    const td = el("td", "（空）", "muted");
    td.colSpan = cells.length;
    tr.append(td);
    tbody.append(tr);
    return;
  }
  for (const record of records) {
    const tr = el("tr");
    for (const cell of cells) tr.append(el("td", cell(record)));
    tbody.append(tr);
  }
}

async function refreshTables() {
  const [orders, positions] = await Promise.all([getJson(API_PATHS.orders), getJson(API_PATHS.positions)]);
  fillTable("orders-table", orders.body, [
    (r) => r.instrument || r.instrument_id || "—",
    (r) => r.side || "—",
    (r) => r.status || "—",
    (r) => money(r.quantity_raw ?? r.quantity),
    (r) => money(r.filled_raw ?? r.filled_quantity_raw),
  ]);
  fillTable("positions-table", positions.body, [
    (r) => r.instrument || r.instrument_id || "—",
    (r) => r.side || "—",
    (r) => money(r.quantity_raw ?? r.quantity),
    (r) => money(r.cost_raw),
    (r) => money(r.market_value_raw),
  ]);
}

async function refreshReadModels() {
  const [ledger, recon, sched, audit] = await Promise.all([
    getJson(API_PATHS.ledger),
    getJson(API_PATHS.reconcileReports),
    getJson(API_PATHS.schedulerRuns),
    getJson(API_PATHS.controlAudit),
  ]);
  const count = (r) => (Array.isArray(r.body) ? `${r.body.length} 条` : `HTTP ${r.status}`);
  rows($("readmodels-rows"), [
    ["/account/ledger", count(ledger)],
    ["/reconcile/reports", count(recon)],
    ["/scheduler/runs", count(sched)],
    ["/control/audit", count(audit)],
  ]);
  // 控制面②③两阶段从这份审计里推进：受理之后命令走到哪一步，这里看得见。
  advancePhases(audit.body);
  renderControl();
}

// ---- 控制面（M3'）：受理 → 执行者判定 → 终态退场 ----
//
// 写面只有这一条。载荷形状就是 `qx-control::ControlCommand`：审计字段
// （request_id / operator_id / reason / target）一个都不能空，服务端 `validate()` 会拒。
//
// 身份边界：`operator_id` 是**审计字段**，不是认证。启用访问策略的部署里，服务端会用
// mTLS 认证边界上的身份覆盖它（`ApiService::submit_command`），并可能直接回
// `403 authenticated_operator_required`——那时页面不能自声明身份，只能如实转述。
async function postJson(path, payload) {
  const response = await fetch(state.base + path, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify(payload),
  });
  let body = null;
  try {
    body = await response.json();
  } catch (_) {
    body = null;
  }
  return { status: response.status, ok: response.ok, body };
}

function controlCommandFromForm() {
  const requestId = $("cmd-request").value.trim() || `console-${Date.now()}`;
  const command = {
    command_id: state.commandSeq,
    request_id: requestId,
    operator_id: $("cmd-operator").value.trim(),
    reason: $("cmd-reason").value.trim(),
    kind: $("cmd-kind").value,
    target: $("cmd-target").value.trim(),
    payload: {},
    permission: $("cmd-permission").value,
    dry_run: $("cmd-dry-run").checked,
  };
  state.commandSeq += 1;
  return command;
}

async function submitCommand(event) {
  if (event) event.preventDefault();
  if (!state.base) return;
  const command = controlCommandFromForm();
  const result = await postJson(API_PATHS.controlCommands, command);
  // 阶段①受理：202 是"收下并落了审计"，不是"已经执行"。4xx/5xx 各有自己的语义，
  // 页面照实印出来，不把任何一种折成"成功"。
  state.lastCommand = { command, result, phases: [describeAccepted(result)] };
  renderControl();
  // 受理之后立刻回读审计，把②③两阶段推进一格。
  await refreshReadModels();
}

function describeAccepted(result) {
  if (result.status === 202) return { phase: PHASES[0], state: "accepted", detail: `202 ${result.body && result.body.status ? result.body.status : "Accepted"}` };
  if (result.status === 403) return { phase: PHASES[0], state: "refused", detail: "403 身份必须来自 mTLS 认证边界（authenticated_operator_required），页面不能自声明 operator" };
  if (result.status === 400) return { phase: PHASES[0], state: "refused", detail: "400 载荷非法（审计字段缺失，或该命令类型在本构建里没有派发者）" };
  if (result.status === 409) return { phase: PHASES[0], state: "refused", detail: "409 控制面拒绝（重复 request_id / 未知命令 / 已终态）" };
  if (result.status === 503) return { phase: PHASES[0], state: "refused", detail: "503 队列不可用（control_state_unavailable）" };
  return { phase: PHASES[0], state: "refused", detail: `HTTP ${result.status}` };
}

// 阶段②③从 `/control/audit` 回读：审计里那条记录的 status 就是判定结果，
// 走到 Executed / Failed 即 `CommandStatus::is_final`——命令终态退场。
function advancePhases(audit) {
  if (!state.lastCommand) return;
  const record = Array.isArray(audit)
    ? audit.find((row) => row && row.request_id === state.lastCommand.command.request_id)
    : null;
  const phases = state.lastCommand.phases.slice(0, 1);
  if (!record) {
    phases.push({ phase: PHASES[1], state: "pending", detail: "审计里还没有这条命令的记录" });
    phases.push({ phase: PHASES[2], state: "pending", detail: "—" });
    state.lastCommand.phases = phases;
    return;
  }
  const status = record.status;
  const final = FINAL_STATUSES.includes(status);
  phases.push({
    phase: PHASES[1],
    state: status === "Accepted" && !final ? "decided" : "decided",
    detail: `command_id=${record.command_id} result_code=${record.result_code || "—"}`,
  });
  phases.push({
    phase: PHASES[2],
    state: final ? "final" : "pending",
    detail: final ? `${status}（终态，已退场）` : `${status}（未到终态，仍在审计里）`,
  });
  state.lastCommand.phases = phases;
}

function renderControl() {
  const container = $("control-rows");
  if (!state.lastCommand) {
    rows(container, [["状态", "未提交（写面只有 POST /control/commands）"]]);
    return;
  }
  const pairs = state.lastCommand.phases.map((entry) => [entry.phase, entry.detail]);
  pairs.push(["status", String(state.lastCommand.result.status)]);
  rows(container, pairs);
}

async function refreshEvents() {
  const res = await getJson(`${API_PATHS.events}?after=${state.cursor}`);
  if (res.status === 409) {
    rows($("events-meta"), [["游标", "409 event_cursor_requires_snapshot —— 需要先取快照重定基"]]);
    return;
  }
  const batch = Array.isArray(res.body) ? res.body : [];
  for (const event of batch) {
    state.events.push(event);
    if (typeof event.seq === "number") state.cursor = Math.max(state.cursor, event.seq + 1);
  }
  if (state.events.length > 500) state.events = state.events.slice(-500);
  renderEvents();
}

function renderEvents() {
  rows($("events-meta"), [
    ["游标 after", state.cursor],
    ["已收事件", state.events.length],
    ["WS", state.wsAlive ? "已连接" : "未连接"],
  ]);
  const tbody = $("events-table").querySelector("tbody");
  tbody.replaceChildren();
  for (const event of state.events.slice(-200).reverse()) {
    const tr = el("tr");
    tr.append(el("td", event.seq));
    tr.append(el("td", event.kind));
    tr.append(el("td", event.ts));
    tr.append(el("td", event.correlation_id || "—"));
    tbody.append(tr);
  }
}

async function refreshAll() {
  if (!state.base) return;
  $("last-refresh").textContent = `刷新于 ${new Date().toLocaleTimeString("zh-CN")}`;
  try {
    await refreshHealth();
    await refreshSnapshot();
    await refreshBalances();
    await refreshTables();
    await refreshReadModels();
    await refreshEvents();
    setConnState("已连接", "pill-ok");
  } catch (error) {
    setConnState("读取失败", "pill-bad");
    rows($("health-rows"), [["错误", String(error && error.message ? error.message : error)]]);
  }
}

// ---- WebSocket 增量（M2'）----

function openSocket() {
  if (!state.base) return;
  closeSocket();
  let socket;
  try {
    socket = new WebSocket(wsUrl(state.base));
  } catch (error) {
    $("ws-state").textContent = `WS 打开失败：${error}`;
    return;
  }
  state.ws = socket;
  socket.onopen = () => {
    state.wsAlive = true;
    $("ws-state").textContent = "WS 已连接";
    renderEvents();
  };
  socket.onclose = () => {
    state.wsAlive = false;
    $("ws-state").textContent = "WS 已断开";
    renderEvents();
    // 断线重连：3 秒后再试，只要还连着 API 就保持实时面。
    if (state.connected) setTimeout(() => state.connected && openSocket(), 3000);
  };
  socket.onerror = () => {
    state.wsAlive = false;
    $("ws-state").textContent = "WS 错误";
  };
  socket.onmessage = (message) => {
    let frame;
    try {
      frame = JSON.parse(message.data);
    } catch (_) {
      return;
    }
    handleFrame(frame);
  };
}

function handleFrame(frame) {
  const type = frame && frame.type;
  if (type === "resync_required") {
    // 服务端游标失效：按协议重取快照、把游标归零，避免拿着旧游标继续读。
    state.cursor = 0;
    $("ws-state").textContent = "WS 要求重同步（已重置游标）";
    refreshAll();
    return;
  }
  if (type === "idle_timeout" || type === "server_shutdown") {
    state.wsAlive = false;
    $("ws-state").textContent = `WS ${type}`;
    renderEvents();
    return;
  }
  const event = frame && frame.event ? frame.event : frame;
  if (event && typeof event.seq === "number") {
    state.events.push(event);
    state.cursor = Math.max(state.cursor, event.seq + 1);
    if (state.events.length > 500) state.events = state.events.slice(-500);
    renderEvents();
  }
}

function closeSocket() {
  if (state.ws) {
    try {
      state.ws.onclose = null;
      state.ws.close();
    } catch (_) {
      /* 关闭失败无所谓 */
    }
    state.ws = null;
  }
  state.wsAlive = false;
}

// ---- 连接生命周期 ----

function connect() {
  const base = normalizeBase($("api-base").value);
  if (!base) return;
  state.base = base;
  state.connected = true;
  localStorage.setItem("qx.console.base", base);
  $("api-base").value = base;
  $("disconnect").disabled = false;
  setConnState("连接中…", "pill-idle");
  refreshAll();
  openSocket();
  if (state.timer) clearInterval(state.timer);
  state.timer = setInterval(refreshAll, POLL_MS);
}

function disconnect() {
  state.connected = false;
  state.base = "";
  if (state.timer) clearInterval(state.timer);
  state.timer = null;
  closeSocket();
  setConnState("未连接", "pill-idle");
  $("disconnect").disabled = true;
  $("ws-state").textContent = "WS 未连接";
  state.lastCommand = null;
  renderControl();
  for (const id of ["health-rows", "snapshot-rows", "balances-rows", "readmodels-rows", "events-meta"]) {
    rows($(id), [["状态", "未连接"]]);
  }
}

window.addEventListener("DOMContentLoaded", () => {
  const saved = localStorage.getItem("qx.console.base");
  if (saved) $("api-base").value = saved;
  $("connect").addEventListener("click", connect);
  $("disconnect").addEventListener("click", disconnect);
  $("control-form").addEventListener("submit", submitCommand);
  $("api-base").addEventListener("keydown", (event) => {
    if (event.key === "Enter") connect();
  });
});
