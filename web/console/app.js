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
  appValidateDataset: "/app/validate-dataset",
  appBacktest: "/app/backtest",
  appVerify: "/app/verify",
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
// ws.rs。这里借用 /events/live 作为通道名，路径本身不参与服务端判定——通道名取自
// API_PATHS，不在第二处再抄一遍字面量。
const WS_PATH = API_PATHS.eventsLive;

const POLL_MS = 5000;
const FETCH_TIMEOUT_MS = 15000;

const state = {
  base: "",
  timer: null,
  refreshInFlight: false,
  ws: null,
  wsAlive: false,
  // 连接拓扑：同源 BFF（页面就是这一层发的）与跨源直连 qx-api 是两条不同的路。
  // BFF 只代理 HTTP 请求，不转发 WebSocket 升级，也不允许页面持有凭据以外的东西。
  viaBff: false,
  // 游标 = 「已经看到的那一条的序号」，`null` 表示还没有基线（下一轮不带 `after`，从头读）。
  // 不能拿 0 当「没有基线」：事件序号是从 0 起的（`crates/qx-runtime/tests/event_log_open_faces.rs`
  // 钉住 `events()[0].seq == 0`），而空日志的 `next_seq` 也是 0——同一个 0 在两边各有解释，
  // 服务端就会把「还没基线」读成「你超前了」并回 409。
  cursor: null,
  baselineSeq: null,
  events: [],
  wsFramesSeen: 0,
  wsUnknownFrames: [],
  shapeIssues: [],
  cycleNon200: 0,
  connected: false,
  commandSeq: 1,
  lastCommand: null,
};

const $ = (id) => document.getElementById(id);

// 后端所有 `*_raw` 都是 1e9 定点（`crates/qx-core/src/numeric.rs` 的 `SCALE`）。
// 页面必须自己还原标度，否则 50000  USDT 会印成 50000000000000。这里是读侧唯一的
// 还原出口：别处不得再除一次，也不得把定点整数当金额直接印出来。
const RAW_DECIMALS = 9;
const RAW_BASE = 10n ** BigInt(RAW_DECIMALS);

// 未算出的钱是 null，不是 0：这里如实显示为 —，不折成 0（与后端 null ≠ 0 同口径）。
function money(value) {
  if (value === null || value === undefined || value === "") return "—";
  let raw;
  try {
    raw = BigInt(value);
  } catch (_) {
    return String(value);
  }
  const negative = raw < 0n;
  const unsigned = negative ? -raw : raw;
  const whole = unsigned / RAW_BASE;
  const fraction = unsigned % RAW_BASE;
  // 小数只留 4 位并去尾零：9 位全打出来没人读，四舍五入又会让同一格在不同刷新之间抖。
  const fractionText = String(fraction).padStart(RAW_DECIMALS, "0").slice(0, 4).replace(/0+$/, "");
  const wholeText = whole.toString().replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return `${negative ? "-" : ""}${wholeText}${fractionText ? "." + fractionText : ""}`;
}

// 价格那一格用 0 表达"没有"（`wire.rs` 的 PositionSnapshot 注释交代了这条纪律：
// 价格是定点数，0 不是合法价格），所以 0 要显示成 —，而钱那一格 0 就是 0。
function price(raw) {
  if (raw === null || raw === undefined) return "—";
  return BigInt(raw) === 0n ? "—" : money(raw);
}

function instrumentText(value) {
  if (!value) return "—";
  if (typeof value === "string") return value;
  // `InstrumentId` 的 serde 形状是 {"symbol":"BTCUSDT-PERP","venue":"BINANCE"}，
  // 直接 String() 会得到 [object Object]——标的列就此永久读不出东西。
  const symbol = value.symbol ?? "";
  const venue = value.venue && (value.venue.venue ?? value.venue);
  return venue ? `${symbol}.${venue}` : String(symbol || "—");
}

// `EventKind` 是外部标签枚举：`{"MarketQuote":{...}}` 或 `"Settle"`。
// 页面只印变体名，把载荷留在事件体里，否则整列都是 [object Object]。
function kindText(kind) {
  if (kind === null || kind === undefined) return "—";
  if (typeof kind === "string") return kind;
  if (typeof kind === "object") {
    const variant = Object.keys(kind)[0];
    return variant || "—";
  }
  return String(kind);
}

// 时间轴是 epoch 毫秒（`crates/qx-core/src/clock.rs` 的 `Ts`，与 `runtime_timestamp_ms()`
// 同源；纳秒只存在于撮合延迟配置里，不在这条轴上）。不还原标度就是一串 13 位整数。
function tsText(value) {
  if (value === null || value === undefined) return "—";
  let raw;
  try {
    raw = BigInt(value);
  } catch (_) {
    return String(value);
  }
  if (raw === 0n) return "—";
  return new Date(Number(raw)).toLocaleString("zh-CN", { hour12: false });
}

// JSON 的 Number 只能安全表示到 2^53−1，而 1e9 定点的金额轻易就超过它
// （1e7 USDT 的权益 raw 就是 1e16）。`JSON.parse` 之后精度已经丢了，reviver 救不回来，
// 所以只能在解析之前把字符串外的大整数字面量换成字符串。读侧一律走 BigInt(String())。
function parseJsonLossless(text) {
  let out = "";
  let inString = false;
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (inString) {
      out += ch;
      if (ch === "\\") {
        i += 1;
        out += text[i] ?? "";
      } else if (ch === '"') {
        inString = false;
      }
      continue;
    }
    if (ch === '"') {
      out += ch;
      inString = true;
      continue;
    }
    if (ch === "-" || (ch >= "0" && ch <= "9")) {
      const match = /^-?\d+/.exec(text.slice(i));
      const literal = match[0];
      i += literal.length - 1;
      const next = text[i + 1];
      const isFloat = next === "." || next === "e" || next === "E";
      const significant = literal.replace("-", "").replace(/^0+(?=\d)/, "");
      if (significant.length >= 16 && !isFloat) out += `"${literal}"`;
      else out += literal;
      continue;
    }
    out += ch;
  }
  return JSON.parse(out);
}

function errorOf(body) {
  if (!body || typeof body !== "object") return "";
  return String(body.error ?? body.detail ?? body.status ?? "");
}

// 序号一类的整数也要过 `parseJsonLossless`，16 位以上的字面量会被换成字符串以保住精度，
// 所以取数不能只认 `typeof === "number"`：那会让一个真的存在的大序号读成"没有基线"。
// 数字照取，数字字符串按 Number 还原，缺失或非法才是 null。
function asNumber(value) {
  if (typeof value === "number") return Number.isFinite(value) ? value : null;
  if (typeof value === "string") {
    const trimmed = value.trim();
    if (trimmed !== "" && Number.isFinite(Number(trimmed))) return Number(trimmed);
  }
  return null;
}

// 后端点名发出、且页面真的读的字段名册。门禁 `web_console_check` 双向核对：
// 这里的每个名字都必须在后端线格式里真的声明过（`wire.rs` / `qx-protocol` / `qx-core::event`），
// 反方向也核——app.js 里出现的每个 `*_raw` 都必须在这份名册里。
// 于是"后端改了字段名"与"前端凭空读一列"都在门禁当场变红，而不是等用户看到一列永久的 —。
const RESPONSE_FIELDS = {
  snapshotTop: ["protocol", "schema_version", "header", "cash_raw", "equity_raw", "available_raw", "margin_raw", "frozen_raw", "realized_pnl_raw", "unrealized_pnl_raw", "fees_raw", "funding_raw", "positions", "orders", "fills", "transfers", "reconcile"],
  snapshotHeader: ["snapshot_id", "account_id", "portfolio_id", "venue_id", "trading_day", "as_of", "event_seq", "state_hash"],
  orderRow: ["order_id", "client_order_id", "instrument", "side", "quantity_raw", "filled_raw", "status"],
  positionRow: ["instrument", "quantity_raw", "today_quantity_raw", "average_price_raw", "mark_price_raw", "unrealized_pnl_raw", "margin_raw"],
  balances: ["cash_raw", "equity_raw", "available_raw", "margin_raw"],
  eventRow: ["seq", "ts", "prio", "kind", "receive_time", "engine_time", "source_seq", "correlation_id", "metadata"],
  envelope: ["schema_version", "kind", "tenant_id", "run_id", "account_id", "portfolio_id", "venue_id", "as_of", "event_seq", "cursor", "state_hash", "source", "lineage", "data"],
};

// 每张表读哪几列、按哪个名册核对形状。列名必须落在对应名册里，`<th>` 的个数与
// `columns` 逐个对齐——门禁 web_console_check 双向核对这两处，所以"后端从来没有这一列"
// 与"页面多开了一列表头"都在门禁变红，而不是等用户看到一列永久的 —。
const TABLE_SPECS = {
  "orders-table": { roster: "orderRow", columns: ["instrument", "side", "status", "quantity_raw", "filled_raw"] },
  "positions-table": { roster: "positionRow", columns: ["instrument", "quantity_raw", "today_quantity_raw", "average_price_raw", "mark_price_raw", "unrealized_pnl_raw", "margin_raw"] },
  "events-table": { roster: "eventRow", columns: ["seq", "kind", "ts", "correlation_id"] },
};

// 价格是定点数而 0 不是合法价格（`wire.rs` 的 PositionSnapshot 交代过这条纪律），
// 所以这两列走 `price`；其余 `*_raw` 是钱，走 `money`。
const PRICE_COLUMNS = ["average_price_raw", "mark_price_raw"];
const TS_COLUMNS = ["ts", "as_of", "receive_time", "engine_time"];

function cellText(key, value) {
  if (key === "instrument") return instrumentText(value);
  if (key === "kind") return kindText(value);
  if (TS_COLUMNS.includes(key)) return tsText(value);
  if (PRICE_COLUMNS.includes(key)) return price(value);
  if (key.endsWith("_raw")) return money(value);
  if (value === null || value === undefined || value === "") return "—";
  return String(value);
}

// 名册里声明过、这一份响应里却没有的字段。空数组就是形状齐全。
function missingFields(roster, record) {
  const names = RESPONSE_FIELDS[roster] || [];
  if (!record || typeof record !== "object") return names.slice();
  return names.filter((name) => !(name in record));
}

function shapeText(missing) {
  return missing.length === 0 ? "齐全" : `缺 ${missing.join(", ")}（后端线格式与本页面名册不再一致）`;
}

// 形状缺口记在一本**去重且有上界**的账上（20 条）：一条不匹配的形状每帧都重复登记，
// 页面就会随运行时长无限增长——那是把后端的漂移换成前端的泄漏。
function noteMissing(roster, record, origin) {
  const missing = missingFields(roster, record);
  if (missing.length === 0) return;
  const issue = `${origin} → ${roster}：${missing.join(", ")}`;
  if (state.shapeIssues.length < 20 && !state.shapeIssues.includes(issue)) state.shapeIssues.push(issue);
}

function el(tag, text, cls) {
  const node = document.createElement(tag);
  if (text !== undefined && text !== null) node.textContent = String(text);
  if (cls) node.className = cls;
  return node;
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

// 入口 URL 带 `?token=` 只有同源 BFF 一种解释：静态目录服务不消费这个查询参数，
// 而 BFF 的监听地址就是发这份页面的地址。所以那一形态下直接预填本源，省掉
// 「从启动横幅抄地址粘回来」这一步；静态形态仍由运维自己点名 API 源（预填会把
// 静态服务的源误当成 API 源连上去）。
function bffEntryBase() {
  try {
    if (!new URLSearchParams(location.search).has("token")) return "";
  } catch (_) {
    return "";
  }
  return location.origin && location.origin !== "null" ? location.origin : "";
}

// 静态控制台没有同源 BFF、CSRF token 或服务端会话；能守住的实际边界是“只允许本机回环入口”。
// 这不等于生产化完成：生产/共享环境必须先做同源代理与认证授权，再暴露给非本机客户端。
function isLoopbackControlTarget(base) {
  let parsed;
  try {
    parsed = new URL(base);
  } catch (_) {
    return false;
  }
  return parsed.protocol === "http:" && (parsed.hostname === "127.0.0.1" || parsed.hostname === "localhost");
}

function renderLocalOnlyRejected() {
  setConnState("已拒绝", "pill-bad");
  rows($("health-rows"), [["安全边界", "静态控制台仅允许 127.0.0.1 / localhost；没有同源 BFF、CSRF 或会话授权，不能连接远端 API"]]);
}

// 同源 BFF 对**任何非 GET** 请求都要求 `X-QX-CSRF` 与它那枚会话记住的值逐字符相等
// （`crates/qx-api/src/console.rs` 的 csrf_matches）。那枚 cookie 刻意**不是** HttpOnly，
// 原因就是双提交模式要页面把它读出来放回请求头——页面不读它，写面就是一条永久 403 的死路。
// 名字两侧都得对上，所以门禁按 console.rs 的 `CONSOLE_CSRF_COOKIE` / `CONSOLE_CSRF_HEADER`
// 常量核对下面这两个字面量。
const CSRF_COOKIE_NAME = "qx_console_csrf";
const CSRF_HEADER_NAME = "X-QX-CSRF";

function csrfToken() {
  const raw = document.cookie || "";
  for (const pair of raw.split(";")) {
    const eq = pair.indexOf("=");
    if (eq < 0) continue;
    if (pair.slice(0, eq).trim() !== CSRF_COOKIE_NAME) continue;
    try {
      return decodeURIComponent(pair.slice(eq + 1).trim());
    } catch (_) {
      return pair.slice(eq + 1).trim();
    }
  }
  return "";
}

async function readJson(response) {
  let body = null;
  try {
    body = parseJsonLossless(await response.text());
  } catch (_) {
    body = null;
  }
  return body;
}

async function fetchWithTimeout(url, init) {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), FETCH_TIMEOUT_MS);
  try {
    return await fetch(url, { ...init, signal: controller.signal });
  } finally {
    clearTimeout(timeout);
  }
}

async function getJson(path) {
  const response = await fetchWithTimeout(state.base + path, { headers: { Accept: "application/json" } });
  const body = await readJson(response);
  if (!response.ok) state.cycleNon200 += 1;
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
  if (!snap.ok) {
    // 404「该账户还没有投影」与 400「键形状非法」/429/503 是四件事，折成同一句就读不回来了。
    rows($("snapshot-rows"), [["状态", `HTTP ${snap.status}${errorOf(snap.body) ? " " + errorOf(snap.body) : ""}`]]);
    return;
  }
  const b = snap.body || {};
  // `AccountSnapshot::to_json()` 把身份与序号放进 `"header":{...}`，只有 `schema_version`
  // 在顶层（`crates/qx-protocol/src/lib.rs` 的那个 format! 字面量）。按顶层读那四格
  // 会永久印空白——不是"没数据"，是读错了层。
  const header = b.header || {};
  state.baselineSeq = asNumber(header.event_seq);
  rows($("snapshot-rows"), [
    ["schema_version", b.schema_version],
    ["snapshot_id", header.snapshot_id],
    ["account_id", header.account_id],
    ["portfolio_id", header.portfolio_id],
    ["venue_id", header.venue_id],
    ["trading_day", header.trading_day],
    ["as_of", tsText(header.as_of)],
    ["event_seq（游标基线）", header.event_seq],
    ["state_hash", header.state_hash],
    ["字段核对", shapeText([
      ...missingFields("snapshotTop", b),
      ...missingFields("snapshotHeader", header),
    ])],
  ]);
}

async function refreshBalances() {
  const bal = await getJson(API_PATHS.balances);
  if (!bal.ok) {
    rows($("balances-rows"), [["状态", `HTTP ${bal.status}${errorOf(bal.body) ? " " + errorOf(bal.body) : ""}`]]);
    return;
  }
  const b = bal.body || {};
  // cash_raw 是按币种的 map，其余三格是标量；`available_raw` / `margin_raw` 可以为 null
  // （"交易所没报这一项"），那要显示 —，不能折成 0。
  const cash = b.cash_raw && typeof b.cash_raw === "object" && !Array.isArray(b.cash_raw)
    ? Object.entries(b.cash_raw).map(([k, v]) => `${k}=${money(v)}`).join("  ") || "（无现金记录）"
    : money(b.cash_raw);
  rows($("balances-rows"), [
    ["cash_raw", cash],
    ["equity_raw", money(b.equity_raw)],
    ["available_raw", money(b.available_raw)],
    ["margin_raw", money(b.margin_raw)],
    ["字段核对", shapeText(missingFields("balances", b))],
  ]);
}

function fillTable(tableId, response) {
  const spec = TABLE_SPECS[tableId];
  const tbody = $(tableId).querySelector("tbody");
  tbody.replaceChildren();
  const colspan = Math.max(spec.columns.length, 1);
  // 非 200 的正文是 `{error: ...}` 对象而不是数组。此前它落到「（空）」那一格，
  // 于是"这个账户确实没有持仓"与"这一列根本没读到"在页面上长得一样——那是最坏的一种误读。
  if (!response.ok) {
    const tr = el("tr");
    const td = el("td", `HTTP ${response.status}${errorOf(response.body) ? " " + errorOf(response.body) : ""}（读取失败，不是空表）`, "muted");
    td.colSpan = colspan;
    tr.append(td);
    tbody.append(tr);
    return;
  }
  const records = response.body;
  if (!Array.isArray(records)) {
    const tr = el("tr");
    const td = el("td", "响应不是数组（形状与端点表承诺不符）", "muted");
    td.colSpan = colspan;
    tr.append(td);
    tbody.append(tr);
    return;
  }
  if (records.length === 0) {
    const tr = el("tr");
    const td = el("td", "（空）", "muted");
    td.colSpan = colspan;
    tr.append(td);
    tbody.append(tr);
    return;
  }
  // 形状核对放在渲染之前：后端改了一个字段名，整表都要说出来，而不是让每一格安静地变成 —。
  const missing = missingFields(spec.roster, records[0]);
  if (missing.length > 0) {
    const tr = el("tr");
    const td = el("td", `字段核对：${shapeText(missing)}`, "muted");
    td.colSpan = colspan;
    tr.append(td);
  }
  for (const record of records) {
    const tr = el("tr");
    for (const key of spec.columns) tr.append(el("td", cellText(key, record ? record[key] : null)));
    tbody.append(tr);
  }
}

async function refreshTables() {
  const [orders, positions] = await Promise.all([getJson(API_PATHS.orders), getJson(API_PATHS.positions)]);
  fillTable("orders-table", orders);
  // 持仓列取的是 `PositionSnapshot` 真的发出来的那七格。后端从来没有 `side` / `cost_raw` /
  // `market_value_raw`（wire.rs），而页面此前照着它们开了三列——那三列永久是 —。
  // 市值也不在页面里现算：那是「各处手抄折算」要防的事，属于后端估值单点。
  fillTable("positions-table", positions);
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
  return postJsonText(path, JSON.stringify(payload));
}

async function postJsonText(path, body) {
  const headers = { "Content-Type": "application/json", Accept: "application/json" };
  const csrf = csrfToken();
  if (csrf) headers[CSRF_HEADER_NAME] = csrf;
  const response = await fetchWithTimeout(state.base + path, {
    method: "POST",
    headers,
    body,
  });
  return { status: response.status, ok: response.ok, body: await readJson(response) };
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
  if (!state.base || !isLoopbackControlTarget(state.base)) return;
  const command = controlCommandFromForm();
  const result = await postJson(API_PATHS.controlCommands, command);
  // 阶段①受理：202 是"收下并落了审计"，不是"已经执行"。4xx/5xx 各有自己的语义，
  // 页面照实印出来，不把任何一种折成"成功"。
  state.lastCommand = { command, result, phases: [describeAccepted(result)] };
  renderControl();
  // 受理之后立刻回读审计，把②③两阶段推进一格。
  await refreshReadModels();
}

// ---- 研究面板：校验 → 回测 → 复核 ----
async function runResearch(validateOnly) {
  const output = $("research-output");
  const buttons = [$("research-validate"), $("research-run")];
  if (!state.base) return;
  for (const button of buttons) button.disabled = true;
  output.textContent = "正在校验数据集…";
  try {
    const specText = $("research-spec").value.trim();
    const spec = JSON.parse(specText);
    const dataset = {
      schema_version: 1,
      dataset_id: spec.run_id || "console-dataset",
      bars_path: spec.bars_path,
    };
    const validation = await postJson(API_PATHS.appValidateDataset, dataset);
    if (!validation.ok || !validation.body?.usable) {
      output.textContent = JSON.stringify({ validation }, null, 2);
      return;
    }
    if (validateOnly) {
      output.textContent = JSON.stringify({ validation }, null, 2);
      return;
    }
    output.textContent = "数据可用，正在回测…";
    // 保留 i128 *_raw 字面量原文；JSON.parse/ stringify 会把大整数舍入到 IEEE-754 精度。
    const backtest = await postJsonText(API_PATHS.appBacktest, specText);
    if (!backtest.ok) {
      output.textContent = JSON.stringify({ validation, backtest }, null, 2);
      return;
    }
    output.textContent = "回测完成，正在复核四份产物…";
    const verification = await postJson(API_PATHS.appVerify, backtest.body);
    output.textContent = JSON.stringify({ validation, backtest, verification }, null, 2);
  } catch (error) {
    output.textContent = error instanceof SyntaxError
      ? `回测规格 JSON 无效：${error.message}`
      : `研究请求失败：${error}`;
  } finally {
    for (const button of buttons) button.disabled = false;
  }
}

function describeAccepted(result) {
  const refusal = errorOf(result.body);
  if (result.status === 202) return { phase: PHASES[0], state: "accepted", detail: `202 ${result.body && result.body.status ? result.body.status : "Accepted"}` };
  // 403 有两种完全不同的来路，混成一句话会让人去改错的地方：
  // 直连 qx-api 时是身份边界（operator 必须来自 mTLS 认证边界，页面不能自声明），
  // 走同源 BFF 时是双提交/同源两道锁没对上（页面要带 X-QX-CSRF，Origin 要等于 Host）。
  if (result.status === 403 && refusal === "console_csrf_token_invalid") {
    return { phase: PHASES[0], state: "refused", detail: "403 console_csrf_token_invalid：同源 BFF 的双提交没对上——页面没读到 X-QX-CSRF，或那枚 cookie 已随会话轮换。重新用引导入口打开页面" };
  }
  if (result.status === 403 && refusal === "console_origin_not_allowed") {
    return { phase: PHASES[0], state: "refused", detail: "403 console_origin_not_allowed：同源 BFF 要求 Origin 与 Host 逐字符相等（跨源写面不由这一层承担）" };
  }
  if (result.status === 403) return { phase: PHASES[0], state: "refused", detail: "403 身份必须来自 mTLS 认证边界（authenticated_operator_required），页面不能自声明 operator" };
  if (result.status === 401 && refusal === "console_session_required") {
    return { phase: PHASES[0], state: "refused", detail: "401 console_session_required：这一层要先用带 ?token= 的引导入口换取会话 cookie（令牌不进 URL 之外的任何地方）" };
  }
  if (result.status === 400) return { phase: PHASES[0], state: "refused", detail: "400 载荷非法（审计字段缺失，或该命令类型在本构建里没有派发者）" };
  if (result.status === 409) return { phase: PHASES[0], state: "refused", detail: "409 控制面拒绝（重复 request_id / 未知命令 / 已终态）" };
  if (result.status === 503) return { phase: PHASES[0], state: "refused", detail: `503 队列不可用（${refusal || "control_state_unavailable"}）` };
  return { phase: PHASES[0], state: "refused", detail: `HTTP ${result.status}${refusal ? " " + refusal : ""}` };
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
  // 上一版这里写的是 `status === "Accepted" && !final ? "decided" : "decided"`：两支同一个值，
  // 三元表达式只是长得像判定，「待执行者回写」与「已判定」在页面上根本分不开。
  phases.push({
    phase: PHASES[1],
    state: final ? "decided" : "awaiting",
    detail: `${final ? "已判定" : "待执行者回写"}：command_id=${record.command_id} status=${status || "—"} result_code=${record.result_code || "—"}`,
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

// `/events` 的 `after` 是**事件序号**，不是这份投影日志的下标；而 409
// `event_cursor_requires_snapshot` 的语义是"你手里的游标我无法证明连续"。此时把游标归零
// 是错的：`after=0` 在空日志上仍是 CursorAhead（空日志的 next_seq 也是 0），在被裁剪过的日志上
// 仍是 CursorTooOld，于是每一轮都 409，页面永远停在"需要先取快照重定基"这句话上。
// 没有基线就写 `null` 并**不带** `after`（服务端那一臂是"从头给"），有基线才带上那一条的序号。
// 真正能重定基的是 `/account/snapshot/envelope` 的 `event_seq`——快照就是那一刻的事实序号。
async function rebaselineCursor(reason) {
  const env = await getJson(API_PATHS.snapshotEnvelope);
  if (!env.ok) {
    state.cursor = null;
    state.baselineSeq = null;
    rows($("events-meta"), [
      ["游标", `重定基失败：${reason} 之后取 /account/snapshot/envelope 得到 HTTP ${env.status}${errorOf(env.body) ? " " + errorOf(env.body) : ""}（没有快照就没有基线，实时增量本轮不可用）`],
    ]);
    renderEvents();
    return false;
  }
  const seq = asNumber(env.body && env.body.event_seq);
  if (seq === null) {
    state.cursor = null;
    renderEvents("重定基失败：/account/snapshot/envelope 的 event_seq 取不出来");
    return false;
  }
  state.cursor = seq;
  state.baselineSeq = seq;
  const missing = missingFields("envelope", env.body);
  renderEvents(`已按快照 event_seq=${seq} 重定基（触发原因：${reason}）${missing.length ? "；" + shapeText(missing) : ""}`);
  return true;
}

// 游标只在一处推进，口径与服务端一致：`after` 是「已经看到的序号」，不是「下一个要取的序号」。
// 写成 `seq + 1` 的那一形态下，只要追平日志就有 `after >= next_seq`，下一轮必然是 409。
function noteCursor(seq) {
  if (seq === null) return;
  state.cursor = state.cursor === null ? seq : Math.max(state.cursor, seq);
}

async function refreshEvents() {
  // 还没有基线时不带 `after`：服务端那一臂是「从头给」，带上 0 会被读成「你超前了」。
  const res = await getJson(
    state.cursor === null ? API_PATHS.events : `${API_PATHS.events}?after=${state.cursor}`,
  );
  if (res.status === 409) {
    await rebaselineCursor("HTTP /events 409 event_cursor_requires_snapshot");
    return;
  }
  if (!res.ok) {
    renderEvents(`事件读取失败：HTTP ${res.status}${errorOf(res.body) ? " " + errorOf(res.body) : ""}`);
    return;
  }
  // 这一条入口发的是**裸 Event**（9 键，序号在 `seq`）。WebSocket 发的是
  // `ProjectionEnvelope`（14 键，序号在外层 `event_seq`、事件在它的 `data` 里）：
  // 两条形状不同，不能共用一个取数口径，见 `acceptEnvelope`。
  const batch = Array.isArray(res.body) ? res.body : [];
  for (const event of batch) {
    const seq = asNumber(event && event.seq);
    noteCursor(seq);
    state.events.push(event);
    noteMissing("eventRow", event, "HTTP /events");
  }
  if (state.events.length > 500) state.events = state.events.slice(-500);
  renderEvents();
}

function renderEvents(note) {
  rows($("events-meta"), [
    ["游标 after", state.cursor === null ? "尚未取得（下一轮不带 after，从头读）" : state.cursor],
    ["快照基线 event_seq", state.baselineSeq === null ? "尚未取得" : state.baselineSeq],
    ["已收事件", state.events.length],
    ["WS", wsStatusText()],
    ["实时来源", state.viaBff ? "同源 BFF 不转发 WebSocket 升级 → 轮询（5s）" : state.wsFramesSeen > 0 ? `WebSocket（已收 ${state.wsFramesSeen} 帧）` : `轮询（WebSocket 尚无数据帧）`],
    ["未知情帧", state.wsUnknownFrames.length ? [...new Set(state.wsUnknownFrames)].join(", ") : "无"],
    ["形状缺口", state.shapeIssues.length ? state.shapeIssues.join(" / ") : "无"],
    [note ? "本轮说明" : "说明", note || "HTTP /events 是裸 Event，WS 是 ProjectionEnvelope；两者序号口径不同"],
  ]);
  const spec = TABLE_SPECS["events-table"];
  const tbody = $("events-table").querySelector("tbody");
  tbody.replaceChildren();
  if (state.events.length === 0) {
    const tr = el("tr");
    const td = el("td", "（空：这一份投影在游标之后没有新事件）", "muted");
    td.colSpan = Math.max(spec.columns.length, 1);
    tr.append(td);
    tbody.append(tr);
    return;
  }
  for (const event of state.events.slice(-200).reverse()) {
    const tr = el("tr");
    for (const key of spec.columns) tr.append(el("td", cellText(key, event ? event[key] : null)));
    tbody.append(tr);
  }
}

async function refreshAll() {
  if (!state.base || state.refreshInFlight) return;
  state.refreshInFlight = true;
  state.cycleNon200 = 0;
  $("last-refresh").textContent = `刷新于 ${new Date().toLocaleTimeString("zh-CN")}`;
  try {
    await refreshHealth();
    await refreshSnapshot();
    await refreshBalances();
    await refreshTables();
    await refreshReadModels();
    await refreshEvents();
    // "已连接"要说的是整条链路，不是某一个套接字的握手状态：只要有一格按语义非 200，
    // 就把那一格数出来印在徽标上，运维才不会把半条断链读成"页面正常"。
    setConnState(state.cycleNon200 === 0 ? "已连接" : `已连接 · ${state.cycleNon200} 格非 200`, state.cycleNon200 === 0 ? "pill-ok" : "pill-idle");
  } catch (error) {
    setConnState("读取失败", "pill-bad");
    rows($("health-rows"), [["错误", String(error && error.message ? error.message : error)]]);
  } finally {
    state.refreshInFlight = false;
  }
}

// ---- WebSocket 增量（M2'）----

// `crates/qx-api/src/ws.rs` 发射的帧类型词表。页面按这份词表分派，任何未在册的帧
// 记进「未知情帧」而不是静默丢掉——静默丢掉正是上一版的故障：它读 `frame.event` /
// `frame.seq`，而后端发的是 `{"type":"event","data":ProjectionEnvelope}`，
// 于是每一帧都不匹配，徽标却印「WS 已连接」，实时面从来没有通过。
const WS_FRAME_KINDS = ["connected", "snapshot", "event", "events", "resync_required", "idle_timeout", "server_shutdown"];

function wsStatusText() {
  if (state.viaBff) return "不适用（同源 BFF 只代理 HTTP 请求，不转发 WebSocket 升级）";
  if (!state.wsAlive) return "未连接";
  return state.wsFramesSeen > 0 ? `已连接 · 已收 ${state.wsFramesSeen} 帧` : "已连接（尚未收到数据帧）";
}

function markWs(text) {
  $("ws-state").textContent = text;
}

// WS 的数据帧外层是 `ProjectionEnvelope`（14 键）：序号在外层的 `event_seq`，
// 事件体在 `data`。这与 HTTP `/events` 的裸 Event 是两条形状，取数不能混用。
function acceptEnvelope(envelope, origin) {
  if (!envelope || typeof envelope !== "object") {
    state.wsUnknownFrames.push(`${origin}:非对象`);
    renderEvents();
    return;
  }
  noteMissing("envelope", envelope, `WS ${origin}`);
  const seq = asNumber(envelope.event_seq);
  const event = envelope.data;
  if (seq === null || !event || typeof event !== "object") {
    state.wsUnknownFrames.push(`${origin}:缺 event_seq 或 data`);
    renderEvents();
    return;
  }
  state.wsFramesSeen += 1;
  state.events.push(event);
  noteMissing("eventRow", event, `WS ${origin}.data`);
  noteCursor(seq);
  if (state.events.length > 500) state.events = state.events.slice(-500);
  renderEvents();
}

function openSocket() {
  if (!state.base) return;
  closeSocket();
  if (state.viaBff) {
    // BFF 形态下不开套接字：握手会被那一层当成普通 GET 走会话判定（401），
    // 而把它报成"WS 已连接"就是给运维一条假的心跳。实时面退化为 5s 轮询，页面上说清。
    markWs(wsStatusText());
    renderEvents();
    return;
  }
  let socket;
  try {
    socket = new WebSocket(wsUrl(state.base));
  } catch (error) {
    markWs(`WS 打开失败：${error}`);
    return;
  }
  state.ws = socket;
  socket.onopen = () => {
    state.wsAlive = true;
    markWs(wsStatusText());
    renderEvents();
  };
  socket.onclose = () => {
    state.wsAlive = false;
    markWs("WS 已断开");
    renderEvents();
    // 断线重连：3 秒后再试，只要还连着 API 就保持实时面。
    if (state.connected) setTimeout(() => state.connected && openSocket(), 3000);
  };
  socket.onerror = () => {
    state.wsAlive = false;
    markWs("WS 错误");
  };
  socket.onmessage = (message) => {
    let frame;
    try {
      frame = parseJsonLossless(String(message.data));
    } catch (_) {
      state.wsUnknownFrames.push("非 JSON 帧");
      renderEvents();
      return;
    }
    handleFrame(frame);
  };
}

function handleFrame(frame) {
  const type = frame && frame.type;
  if (typeof type !== "string" || !WS_FRAME_KINDS.includes(type)) {
    state.wsUnknownFrames.push(String(type || "无 type"));
    renderEvents();
    return;
  }
  if (type === "resync_required") {
    markWs("WS 要求重同步（按快照 event_seq 重定基）");
    rebaselineCursor("WS resync_required");
    refreshAll();
    return;
  }
  if (type === "idle_timeout" || type === "server_shutdown") {
    state.wsAlive = false;
    markWs(`WS ${type}`);
    renderEvents();
    return;
  }
  if (type === "connected") {
    state.wsFramesSeen += 1;
    renderEvents();
    return;
  }
  if (type === "snapshot") {
    const data = frame.data || {};
    const header = data.header || {};
    noteMissing("snapshotTop", data, "WS snapshot.data");
    const seq = asNumber(header.event_seq);
    if (seq !== null) {
      state.baselineSeq = seq;
      noteCursor(seq);
    }
    state.wsFramesSeen += 1;
    renderEvents();
    return;
  }
  if (type === "event") {
    acceptEnvelope(frame.data, "event");
    return;
  }
  if (type === "events") {
    for (const envelope of Array.isArray(frame.data) ? frame.data : []) {
      acceptEnvelope(envelope, "events");
    }
    return;
  }
  state.wsUnknownFrames.push(type);
  renderEvents();
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
  if (!base) {
    // 空基地址不能静默返回：点了「连接」却什么都没发生，运维会读成"页面坏了"或"已经连上"。
    setConnState("缺少基地址", "pill-bad");
    rows($("health-rows"), [["连接", "请先填 API 基地址：同源 BFF（qx-cli console）填它自己的监听地址，静态形态填 qx-api 的监听地址"]]);
    return;
  }
  if (!isLoopbackControlTarget(base)) {
    renderLocalOnlyRejected();
    return;
  }
  state.base = base;
  state.connected = true;
  // 页面与 API 同源 ⇒ 这一份页面是同源 BFF 发出来的（`qx-cli console`）；不同源 ⇒ 静态件
  // 直连 qx-api，要靠 `api.cors_allowed_origins`。两种形态的实时面与凭据面不一样，
  // 用一条 if 分开说，而不是把 BFF 形态也报成"WS 已连接"。
  const pageOrigin = typeof location !== "undefined" && location.origin && location.origin !== "null" ? location.origin : "";
  state.viaBff = pageOrigin !== "" && base.replace(/\/+$/, "") === pageOrigin;
  state.wsFramesSeen = 0;
  state.wsUnknownFrames = [];
  state.shapeIssues = [];
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
  state.viaBff = false;
  state.cursor = null;
  state.baselineSeq = null;
  state.events = [];
  state.wsFramesSeen = 0;
  state.wsUnknownFrames = [];
  state.shapeIssues = [];
  state.cycleNon200 = 0;
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
  const seeded = saved || bffEntryBase();
  if (seeded) $("api-base").value = seeded;
  $("connect").addEventListener("click", connect);
  $("disconnect").addEventListener("click", disconnect);
  $("control-form").addEventListener("submit", submitCommand);
  $("research-validate").addEventListener("click", () => runResearch(true));
  $("research-run").addEventListener("click", () => runResearch(false));
  $("api-base").addEventListener("keydown", (event) => {
    if (event.key === "Enter") connect();
  });
});
