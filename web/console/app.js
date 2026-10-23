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
  appBacktestRun: "/app/backtest/start",
  appVerify: "/app/verify",
  appResearchArtifacts: "/app/research-artifacts",
  appCompareRuns: "/app/compare-runs",
  appRunExperiment: "/app/run-experiment/start",
  appDepthBacktest: "/app/depth-backtest/start",
  appDepthExperiment: "/app/run-depth-experiment/start",
  appVerifyDepth: "/app/verify-depth",
  appMultiLegBacktest: "/app/multi-leg-backtest/start",
  appMultiLegExperiment: "/app/run-multi-leg-experiment/start",
  appMultiAccountBacktest: "/app/multi-account-backtest/start",
  appMultiAccountDepthBacktest: "/app/multi-account-depth-backtest/start",
  appMultiAccountJointBacktest: "/app/multi-account-joint-backtest/start",
  appMultiMarketBacktest: "/app/multi-market-backtest/start",
  appMultiMarketDepthBacktest: "/app/multi-market-depth-backtest/start",
  appMultiMarketExperiment: "/app/run-multi-market-experiment/start",
  appMultiMarketDepthExperiment: "/app/run-multi-market-depth-experiment/start",
  appRunStatus: "/app/runs/{run_id}",
  appRunCancel: "/app/runs/{run_id}/cancel",
  events: "/events",
  eventsLive: "/events/live",
};

const EXTENDED_RESEARCH_ROUTES = {
  appBacktestRun: API_PATHS.appBacktestRun,
  appCompareRuns: API_PATHS.appCompareRuns,
  appRunExperiment: API_PATHS.appRunExperiment,
  appDepthBacktest: API_PATHS.appDepthBacktest,
  appDepthExperiment: API_PATHS.appDepthExperiment,
  appVerifyDepth: API_PATHS.appVerifyDepth,
  appMultiLegBacktest: API_PATHS.appMultiLegBacktest,
  appMultiLegExperiment: API_PATHS.appMultiLegExperiment,
  appMultiAccountBacktest: API_PATHS.appMultiAccountBacktest,
  appMultiAccountDepthBacktest: API_PATHS.appMultiAccountDepthBacktest,
  appMultiAccountJointBacktest: API_PATHS.appMultiAccountJointBacktest,
  appMultiMarketBacktest: API_PATHS.appMultiMarketBacktest,
  appMultiMarketDepthBacktest: API_PATHS.appMultiMarketDepthBacktest,
  appMultiMarketExperiment: API_PATHS.appMultiMarketExperiment,
  appMultiMarketDepthExperiment: API_PATHS.appMultiMarketDepthExperiment,
};
const EXTENDED_RESEARCH_MAX_WAIT_MS = 30 * 60 * 1000;
const EXTENDED_RESEARCH_POLL_MS = 500;

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
  activeResearchRunId: null,
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
  clearResearchReport();
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
    if (verification.ok && verification.body?.verified === true) {
      renderResearchReport(backtest.body, verification.body);
    }
  } catch (error) {
    output.textContent = error instanceof SyntaxError
      ? `回测规格 JSON 无效：${error.message}`
      : `研究请求失败：${error}`;
  } finally {
    for (const button of buttons) button.disabled = false;
  }
}

// 更新策略 kind 时保留原始 JSON 数字字面量：JSON.parse/stringify 会将 i128 *_raw
// 四舍五入到 IEEE-754 精度，快速选择不能悄悄改写资金、数量或 seed。
function selectResearchStrategy(kind) {
  const editor = $("research-spec");
  const source = editor.value;
  const spec = JSON.parse(source);
  if (!spec || typeof spec.strategy !== "object" || Array.isArray(spec.strategy)) {
    throw new Error("BacktestSpec 缺少 strategy 对象");
  }
  const strategyOffset = source.indexOf('"strategy"');
  const kindMatch = /"kind"\s*:\s*"[^"]+"/.exec(source.slice(strategyOffset));
  if (strategyOffset < 0 || !kindMatch) throw new Error("strategy.kind 字段缺失");
  const start = strategyOffset + kindMatch.index;
  editor.value = source.slice(0, start)
    + kindMatch[0].replace(/"[^"]+"$/, JSON.stringify(kind))
    + source.slice(start + kindMatch[0].length);
}

function syncResearchStrategySelector() {
  try {
    const kind = JSON.parse($("research-spec").value)?.strategy?.kind;
    const selector = $("research-strategy");
    if ([...selector.options].some((option) => option.value === kind)) selector.value = kind;
  } catch {
    // The editor reports malformed JSON when the user validates or runs it.
  }
}

const UNIFIED_EVENT_CACHE_LIMIT = 50000;
const UNIFIED_EVENT_PAGE_SIZE = 300;

const researchReportState = {
  rows: [], kind: "", detailRequest: 0, selectedRow: null,
  latestArtifactView: null, comparisonCurves: [], unifiedReports: [], outcome: null,
  strategyDiagnostics: null, eventPageRequest: 0, accountBatch: null,
  unifiedEventGeneration: 0, unifiedEventLoading: false, unifiedEventPage: 0,
  unifiedEventRows: null, unifiedEventVerifiedCount: 0,
};

function clearResearchReport() {
  researchReportState.rows = [];
  researchReportState.kind = "";
  researchReportState.detailRequest += 1;
  researchReportState.selectedRow = null;
  researchReportState.latestArtifactView = null;
  researchReportState.outcome = null;
  researchReportState.accountBatch = null;
  researchReportState.strategyDiagnostics = null;
  $("research-diagnostic-select-row").hidden = true;
  $("research-diagnostic-select").replaceChildren();
  researchReportState.eventPageRequest += 1;
  researchReportState.comparisonCurves = [];
  researchReportState.unifiedEventGeneration += 1;
  researchReportState.unifiedEventLoading = false;
  researchReportState.unifiedEventPage = 0;
  researchReportState.unifiedEventRows = null;
  researchReportState.unifiedEventVerifiedCount = 0;
  $("research-unified-events-load").disabled = false;
  $("research-unified-events-all").disabled = false;
  $("research-unified-events-next").disabled = true;
  $("research-unified-events-page-prev").disabled = true;
  $("research-unified-events-page-next").disabled = true;
  researchReportState.unifiedReports = [];
  $("card-research-report").hidden = true;
  $("research-report-table").tBodies[0].replaceChildren();
  $("research-unified-events-table").tBodies[0].replaceChildren();
  $("research-unified-events-status").textContent = "";
  $("research-unified-events-page-status").textContent = "";
  $("research-report-plot").replaceChildren();
  $("research-report-detail").textContent = "选择表格行查看";
  $("research-artifact-drilldown").hidden = true;
  $("research-account-detail").hidden = true;
  $("research-account-events").open = false;
  $("research-account-status").textContent = "";
  $("research-account-event-status").textContent = "";
  $("research-account-event-page-status").textContent = "";
  $("research-account-event-filter").value = "";
  $("research-account-event-count").textContent = "";
  $("research-account-event-prev").disabled = true;
  $("research-account-event-next").disabled = true;
  $("research-artifact-status").textContent = "选择已有 Bar 或深度回测结果，读取经复核的净值、成交和行情明细。";
  $("research-diagnostic-file").value = "";
  $("research-diagnostic-result").hidden = true;
  $("research-diagnostic-select-row").hidden = true;
  $("research-diagnostic-select").replaceChildren();
  $("research-diagnostic-import").hidden = false;
  $("research-bars-heading").hidden = false;
  $("research-bars-status").hidden = false;
  $("research-bars-detail").hidden = false;
  $("research-depth-detail").hidden = true;
  $("research-events-detail").hidden = false;
  $("research-timeline-detail").hidden = false;
  $("research-diagnostic-status").textContent = "";
  $("research-diagnostic-table").tBodies[0].replaceChildren();
  $("research-diagnostic-pit-table").tBodies[0].replaceChildren();
  $("research-diagnostic-pit-detail").textContent = "选择一条 PIT 样本查看账本版本";
  $("research-diagnostic-pit-table").tBodies[0].replaceChildren();
  $("research-timeline-status").textContent = "";
  $("research-timeline-filter").value = "";
  $("research-timeline-count").textContent = "";
  $("research-event-page-status").textContent = "";
  $("research-event-prev").disabled = true;
  $("research-event-next").disabled = true;
  $("research-equity-plot").replaceChildren();
  $("research-position-plot").replaceChildren();
  $("research-artifact-metrics").replaceChildren();
  $("research-equity-legend").replaceChildren();
  $("research-equity-comparison-status").textContent = "";
  $("research-unified-status").textContent = "";
  $("research-unified-filter").value = "all";
  for (const id of ["research-unified-table", "research-account-table", "research-account-event-table", "research-equity-table", "research-fill-table", "research-event-table", "research-timeline-table", "research-depth-table", "research-market-depth-table"]) {
    const table = $(id);
    table.tHead.rows[0].replaceChildren();
    table.tBodies[0].replaceChildren();
  }
}

function researchBps(value) {
  const number = asNumber(value);
  return number === null ? "—" : `${(number / 100).toFixed(2)}% (${number} bp)`;
}

function researchMetric(label, value) {
  const card = el("div", null, "research-metric");
  card.append(el("span", label, "label"), el("span", value ?? "—", "value"));
  return card;
}

function renderVerifiedArtifactMetrics(view) {
  const host = $("research-artifact-metrics");
  host.replaceChildren();
  const summary = view.summary ?? {};
  const metrics = summary.metrics ?? summary;
  const rejectionFacts = Array.isArray(summary.rejection_reasons)
    ? summary.rejection_reasons
    : (Array.isArray(summary.rejection_facts)
      ? summary.rejection_facts.map(([reason, count]) => ({ reason, count }))
      : []);
  const rejected = summary.rejected_orders ?? rejectionFacts.reduce((total, item) => total + Number(item.count ?? 0), 0);
  const values = [
    ["收益", metrics.return_bps == null ? null : researchBps(metrics.return_bps)],
    ["最大回撤", metrics.max_drawdown_bps == null ? null : researchBps(metrics.max_drawdown_bps)],
    ["主腿收益", metrics.primary_return_bps == null ? null : researchBps(metrics.primary_return_bps)],
    ["参考腿收益", metrics.reference_return_bps == null ? null : researchBps(metrics.reference_return_bps)],
    ["手续费 raw", metrics.fees_raw],
    ["资金费 raw", metrics.funding_raw],
    ["换手 raw", metrics.turnover_raw],
    ["单位净成本 bp", metrics.cost_bps],
    ["拒单数", rejected],
    ["成交数", summary.fills],
    ["期初资金 raw", summary.account?.initial_cash_raw],
    ["期末权益 raw", metrics.final_equity_raw],
  ];
  for (const [label, value] of values) {
    if (value !== undefined && value !== null) host.append(researchMetric(label, value));
  }
  if (rejectionFacts.length) {
    host.append(researchMetric("拒单原因", rejectionFacts.map((item) => `${item.reason}: ${item.count}`).join("；")));
  }
}

function showResearchDetail(value) {
  const detail = $("research-report-detail");
  detail.textContent = JSON.stringify(value, null, 2);
  detail.closest("details").open = true;
}

function researchOutcomeForRow(row) {
  if (row.source?.batch_id && Array.isArray(row.source?.runs)) return row.source;
  if (row.source?.artifacts) return row.source;
  if (row.source?.artifact_path && row.source?.markets_total !== undefined) return row.source;
  if (row.source?.attribution_artifact) return row.source;
  if (row.source?.outcome?.batch_id && Array.isArray(row.source.outcome?.runs)) return row.source.outcome;
  if (row.source?.outcome?.artifacts) return row.source.outcome;
  if (row.source?.outcome?.artifact_path && row.source.outcome?.markets_total !== undefined) return row.source.outcome;
  if (row.source?.outcome?.attribution_artifact) return row.source.outcome;
  return null;
}

function renderMultiMarketResearch(markets) {
  const panel = $("research-markets-detail");
  const status = $("research-markets-status");
  const barStatus = $("research-market-bars-status");
  panel.hidden = !Array.isArray(markets);
  $("research-market-table").tHead.rows[0].replaceChildren();
  $("research-market-table").tBodies[0].replaceChildren();
  $("research-market-pnl-status").hidden = true;
  $("research-market-pnl-plot").replaceChildren();
  $("research-market-pnl-table").hidden = true;
  $("research-market-pnl-table").tHead.rows[0].replaceChildren();
  $("research-market-pnl-table").tBodies[0].replaceChildren();
  $("research-market-bar-table").tHead.rows[0].replaceChildren();
  $("research-market-bar-table").tBodies[0].replaceChildren();
  $("research-market-depth-table").tHead.rows[0].replaceChildren();
  $("research-market-depth-table").tBodies[0].replaceChildren();
  $("research-market-depth-status").hidden = true;
  $("research-market-depth-detail").hidden = true;
  barStatus.hidden = false;
  $("research-market-bar-table").hidden = false;
  if (!Array.isArray(markets)) return;
  const hasDepth = markets.some((market) => market.depth?.source_status === "verified");
  status.textContent = `${markets.length} 个市场的输入指纹、已提供的数据质量清单和逐市场成交数已复核。点击一行查看该市场的${hasDepth ? "盘口或 Bar" : "Bar"}采样。`;
  const rows = markets.map((market) => ({
    instrument: market.instrument,
    data_tier: market.data_tier ?? "bar",
    rows: market.depth?.total ?? market.bars?.total ?? 0,
    fills: market.fill_count ?? 0,
    pnl_currency: market.pnl_contribution?.currency ?? "—",
    pnl_contribution_raw: market.pnl_contribution?.rows?.at(-1)?.pnl_contribution_raw ?? "—",
    quality: market.quality_status ?? "unknown",
    data_fingerprint: market.data_fingerprint ?? "—",
  }));
  renderResearchObjectTable("research-market-table", rows, ["instrument", "data_tier", "rows", "fills", "pnl_currency", "pnl_contribution_raw", "quality", "data_fingerprint"]);
  const rendered = [...$("research-market-table").tBodies[0].rows];
  const showMarket = (index) => {
    const market = markets[index];
    if (!market) return;
    const contribution = market.pnl_contribution;
    const pnlStatus = $("research-market-pnl-status");
    const pnlTable = $("research-market-pnl-table");
    if (contribution) {
      pnlStatus.hidden = false;
      pnlTable.hidden = false;
      pnlStatus.textContent = `${market.instrument} · ${contribution.currency} · ${contribution.total ?? 0} 个共享权益时点 · 展示 ${contribution.rows?.length ?? 0}${contribution.truncated ? "（有界采样）" : ""}。此标的是已归因盈亏贡献；未分配的初始现金不计入任何标的。`;
      renderResearchObjectTable("research-market-pnl-table", contribution.rows ?? [], ["equity_index", "ts", "pnl_contribution_raw"]);
      renderMarketContributionPlot(market);
    } else {
      pnlStatus.hidden = true;
      $("research-market-pnl-plot").replaceChildren();
      pnlTable.hidden = true;
      pnlTable.tHead.rows[0].replaceChildren();
      pnlTable.tBodies[0].replaceChildren();
    }
    const depth = market.depth?.source_status === "verified" ? market.depth : null;
    barStatus.hidden = Boolean(depth);
    $("research-market-bar-table").hidden = Boolean(depth);
    $("research-market-depth-status").hidden = !depth;
    $("research-market-depth-detail").hidden = !depth;
    if (depth) {
      const depthRows = depth.rows ?? [];
      $("research-market-depth-status").textContent = `${market.instrument} · ${market.data_tier ?? "depth"} · ${depth.total ?? 0} 份盘口快照 · 展示 ${depthRows.length}${depth.truncated ? "（有界采样或档位截断）" : ""} · 数据指纹 ${market.data_fingerprint}`;
      renderResearchObjectTable("research-market-depth-table", depthRows, ["ts", "sequence", "bids", "asks", "levels_truncated"]);
      return;
    }
    const barRows = market.bars?.rows ?? [];
    barStatus.textContent = `${market.instrument} · ${market.bars?.total ?? 0} 根 Bar · 展示 ${barRows.length}${market.bars?.truncated ? "（等距采样）" : ""} · 数据指纹 ${market.data_fingerprint}`;
    renderResearchObjectTable("research-market-bar-table", barRows, ["ts", "open_raw", "high_raw", "low_raw", "close_raw", "volume_raw"]);
  };
  rendered.forEach((row, index) => {
    row.setAttribute("role", "button");
    row.tabIndex = 0;
    row.setAttribute("aria-label", `查看 ${markets[index].instrument} 行情采样`);
    row.onclick = () => showMarket(index);
    row.onkeydown = (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        showMarket(index);
      }
    };
  });
  if (markets.length) showMarket(0);
}

async function loadResearchArtifactDetail(row, strategyDiagnostics = null, explicitOutcome = null) {
  const requestId = ++researchReportState.detailRequest;
  const preservedBatch = explicitOutcome && researchReportState.accountBatch
    ? researchReportState.accountBatch
    : null;
  const outcome = explicitOutcome ?? researchOutcomeForRow(row);
  const panel = $("research-artifact-drilldown");
  const status = $("research-artifact-status");
  researchReportState.latestArtifactView = null;
  researchReportState.outcome = null;
  researchReportState.accountBatch = preservedBatch;
  researchReportState.strategyDiagnostics = null;
  researchReportState.eventPageRequest += 1;
  panel.hidden = false;
  $("research-equity-plot").replaceChildren();
  $("research-position-plot").replaceChildren();
  $("research-artifact-metrics").replaceChildren();
  $("research-account-detail").hidden = !preservedBatch;
  if (!preservedBatch) $("research-account-status").textContent = "";
  const resetTables = ["research-equity-table", "research-fill-table", "research-event-table", "research-timeline-table", "research-market-table", "research-market-bar-table"];
  if (!preservedBatch) resetTables.unshift("research-account-table", "research-account-event-table");
  for (const id of resetTables) {
    const table = $(id);
    table.tHead.rows[0].replaceChildren();
    table.tBodies[0].replaceChildren();
  }
  renderMultiMarketResearch(null);
  $("research-diagnostic-result").hidden = true;
  $("research-diagnostic-select-row").hidden = true;
  $("research-diagnostic-select").replaceChildren();
  $("research-diagnostic-status").textContent = "";
  $("research-diagnostic-table").tBodies[0].replaceChildren();
  $("research-diagnostic-pit-table").tBodies[0].replaceChildren();
  $("research-diagnostic-pit-detail").textContent = "选择一条 PIT 样本查看账本版本";
  $("research-timeline-status").textContent = "";
  $("research-timeline-filter").value = "";
  $("research-timeline-count").textContent = "";
  $("research-event-page-status").textContent = "";
  $("research-event-prev").disabled = true;
  $("research-event-next").disabled = true;
  if (!outcome) {
    status.textContent = "该行没有可钻取的回测产物（比较摘要只含调用方指标）。";
    return;
  }
  status.textContent = "正在读取 " + outcome.run_id + " 的产物并复核…";
  let response;
  try {
    const request = Array.isArray(strategyDiagnostics)
      ? { outcome, market_strategy_diagnostics: strategyDiagnostics }
      : strategyDiagnostics?.account_strategy_diagnostics
        ? { outcome, account_strategy_diagnostics: strategyDiagnostics.account_strategy_diagnostics }
        : strategyDiagnostics
          ? { outcome, strategy_diagnostics: strategyDiagnostics }
          : outcome;
    response = await postJson(API_PATHS.appResearchArtifacts, request);
  } catch (error) {
    if (requestId === researchReportState.detailRequest) status.textContent = "明细请求失败：" + error;
    return;
  }
  if (requestId !== researchReportState.detailRequest) return;
  if (!response.ok) {
    status.textContent = "明细读取失败：" + (response.body?.message ?? response.body?.error ?? response.status);
    return;
  }
  const view = response.body;
  if (!view?.verification?.verified) {
    status.textContent = "产物复核未通过：" + ((view?.verification?.mismatches ?? []).join("；") || "未返回复核细节");
    return;
  }
  if (view.run_kind === "multi_account") {
    renderResearchAccountBatch(view, outcome, strategyDiagnostics?.account_strategy_diagnostics ?? null);
    return;
  }
  renderResearchArtifactDetailView(view, outcome, strategyDiagnostics, preservedBatch?.selectedAccountId ?? null);
}

function renderResearchAccountBatch(view, outcome, diagnosticAttachments = null) {
  const panel = $("research-account-detail");
  const status = $("research-artifact-status");
  $("research-diagnostic-import").hidden = false;
  const accounts = view.accounts ?? [];
  const initialEventOffsets = Object.fromEntries(accounts
    .filter((account) => account.status === "succeeded")
    .map((account) => [account.account_id, 0]));
  researchReportState.accountBatch = {
    view, outcome, selectedAccountId: null,
    diagnosticAttachments,
    eventInstrument: view.event_instrument ?? "",
    eventCursorHistory: [initialEventOffsets], eventCursorIndex: 0, eventPageRequest: 0,
  };
  panel.hidden = false;
  $("research-account-status").textContent = `批次 ${view.run_id} · ${view.summary.accounts_succeeded} / ${view.summary.accounts_total} 个账户成功 · 加权收益 ${view.summary.weighted_return_bps == null ? "—" : researchBps(view.summary.weighted_return_bps)} · ${view.summary.settlement_currency} · 各账户资金、仓位和风险相互独立。`;
  renderResearchObjectTable("research-account-table", accounts, ["account_id", "status", "instrument", "return_bps", "max_drawdown_bps", "fills", "run_id"]);
  const instrumentFilter = $("research-account-event-instrument");
  const instruments = [...new Set(accounts
    .filter((account) => account.status === "succeeded" && account.instrument)
    .map((account) => account.instrument))].sort();
  instrumentFilter.replaceChildren();
  const allInstruments = el("option", "全部标的");
  allInstruments.value = "";
  instrumentFilter.append(allInstruments);
  for (const instrument of instruments) {
    const option = el("option", instrument);
    option.value = instrument;
    instrumentFilter.append(option);
  }
  instrumentFilter.value = instruments.includes(researchReportState.accountBatch.eventInstrument)
    ? researchReportState.accountBatch.eventInstrument
    : "";
  instrumentFilter.disabled = instruments.length < 2;
  renderResearchAccountEventPage(view);
  const rows = [...$("research-account-table").tBodies[0].rows];
  rows.forEach((row, index) => {
    row.classList.add("research-account-row");
    row.tabIndex = 0;
    row.setAttribute("role", "button");
    row.setAttribute("aria-label", `查看账户 ${accounts[index].account_id}`);
    const select = () => selectResearchAccount(index);
    row.addEventListener("click", select);
    row.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        select();
      }
    });
  });
  const requestedAccountId = researchReportState.selectedRow?.parameters?.account_id;
  const requestedIndex = accounts.findIndex((account) => account.account_id === requestedAccountId);
  const firstVerifiedIndex = accounts.findIndex((account) => account.status === "succeeded" && account.report?.verification?.verified);
  const selectedIndex = requestedIndex >= 0 ? requestedIndex : firstVerifiedIndex;
  if (selectedIndex >= 0 && accounts[selectedIndex].status === "succeeded") {
    selectResearchAccount(selectedIndex);
  } else if (selectedIndex >= 0) {
    status.textContent = `账户 ${accounts[selectedIndex].account_id} 未成功；可从上方批次表选择其他成功账户查看明细。`;
    $("research-account-table").tBodies[0].rows[selectedIndex]?.setAttribute("aria-current", "true");
  } else {
    status.textContent = "批次结构已核对，但没有可展开的成功账户。";
  }
}

function researchAccountEventOffset(offsets) {
  return Object.values(offsets ?? {}).reduce((total, value) => total + Number(value ?? 0), 0);
}

function renderResearchAccountEventPage(view) {
  const events = view.events ?? {};
  const rows = researchTimelineRows(events.rows ?? []);
  const batch = researchReportState.accountBatch;
  const eventInstrument = view.event_instrument ?? "";
  if (batch?.view?.run_id === view.run_id) batch.eventInstrument = eventInstrument;
  const offset = Number(events.offset ?? 0);
  const total = Number(events.total ?? 0);
  const end = offset + (events.rows?.length ?? 0);
  const first = events.rows?.length ? offset + 1 : 0;
  $("research-account-event-status").textContent = total
    ? "各账户事件先独立重放校验，再按时间、事件优先级、账户和序号稳定合并"
      + (eventInstrument ? "；当前只显示 " + eventInstrument : "")
      + "；订单、账户和资金事实仍保留各自账户身份。"
    : "所选批次没有可展示的事件记录。";
  $("research-account-event-page-status").textContent = total
    ? `合并事件 ${first}–${end} / ${total}；翻页会重新校验各账户完整事件日志。`
    : "没有事件记录";
  $("research-account-event-prev").disabled = offset <= 0;
  $("research-account-event-next").disabled = !events.truncated;
  renderResearchTimeline(rows, {
    tableId: "research-account-event-table",
    filterId: "research-account-event-filter",
    countId: "research-account-event-count",
  });
}

function resetResearchAccountEventInstrument() {
  const batch = researchReportState.accountBatch;
  const instrumentFilter = $("research-account-event-instrument");
  if (!batch || instrumentFilter.value === (batch.view.event_instrument ?? "")) return;
  const offsets = Object.fromEntries(
    Object.keys(batch.view.account_event_offsets ?? {}).map((accountId) => [accountId, 0]),
  );
  void loadResearchAccountEventPage("reset", offsets);
}

async function loadResearchAccountEventPage(direction, resetOffsets = null) {
  const batch = researchReportState.accountBatch;
  if (!batch) return;
  const currentEvents = batch.view.events ?? {};
  const currentIndex = batch.eventCursorIndex;
  let targetIndex = currentIndex;
  let offsets;
  if (direction === "reset") {
    targetIndex = 0;
    offsets = resetOffsets;
  } else if (direction === "next") {
    if (!currentEvents.truncated) return;
    targetIndex = currentIndex + 1;
    offsets = batch.eventCursorHistory[targetIndex] ?? batch.view.account_event_offsets;
    if (!offsets || researchAccountEventOffset(offsets) <= Number(currentEvents.offset ?? 0)) return;
  } else {
    if (currentIndex <= 0) return;
    targetIndex = currentIndex - 1;
    offsets = batch.eventCursorHistory[targetIndex];
  }
  if (!offsets) return;
  const requestId = ++batch.eventPageRequest;
  const offset = researchAccountEventOffset(offsets);
  $("research-account-event-page-status").textContent = "正在重放并合并校验各账户事件日志…";
  $("research-account-event-prev").disabled = true;
  $("research-account-event-next").disabled = true;
  try {
    const request = {
      outcome: batch.outcome,
      event_page: { offset, limit: 300 },
      account_event_offsets: offsets,
    };
    if (batch.diagnosticAttachments) {
      request.account_strategy_diagnostics = batch.diagnosticAttachments;
    }
    const eventInstrument = $("research-account-event-instrument").value.trim();
    if (eventInstrument) request.event_instrument = eventInstrument;
    const response = await postJson(API_PATHS.appResearchArtifacts, request);
    if (researchReportState.accountBatch !== batch || requestId !== batch.eventPageRequest) return;
    const next = response.body;
    if (!response.ok || !next?.verification?.verified || next.run_id !== batch.view.run_id
      || next.run_kind !== "multi_account") {
      $("research-account-event-page-status").textContent = "跨账户事件页复核失败："
        + (next?.message ?? next?.error ?? response.status ?? "响应结构无效");
      $("research-account-event-instrument").value = batch.view.event_instrument ?? "";
      $("research-account-event-prev").disabled = currentIndex <= 0;
      $("research-account-event-next").disabled = !currentEvents.truncated;
      return;
    }
    if (direction === "reset") {
      batch.eventCursorHistory = [{ ...offsets }];
      batch.eventCursorIndex = 0;
    } else {
      batch.eventCursorHistory[targetIndex] = { ...offsets };
      batch.eventCursorIndex = targetIndex;
    }
    batch.view = next;
    $("research-account-event-instrument").value = next.event_instrument ?? "";
    renderResearchAccountEventPage(next);
  } catch (error) {
    if (researchReportState.accountBatch === batch && requestId === batch.eventPageRequest) {
      $("research-account-event-instrument").value = batch.view.event_instrument ?? "";
      $("research-account-event-page-status").textContent = "跨账户事件页读取失败：" + error;
      $("research-account-event-prev").disabled = currentIndex <= 0;
      $("research-account-event-next").disabled = !currentEvents.truncated;
    }
  }
}

function selectResearchAccount(index) {
  const batch = researchReportState.accountBatch;
  const account = batch?.view.accounts?.[index];
  if (!batch || !account?.report?.verification?.verified) return;
  const member = batch.outcome.runs?.find((run) => run.account_id === account.account_id);
  if (!member?.outcome) return;
  batch.selectedAccountId = account.account_id;
  for (const row of $("research-account-table").tBodies[0].rows) row.removeAttribute("aria-current");
  const selectedRow = $("research-account-table").tBodies[0].rows[index];
  selectedRow?.setAttribute("aria-current", "true");
  renderResearchArtifactDetailView(account.report, member.outcome, null, account.account_id);
}

function renderResearchArtifactDetailView(view, outcome, strategyDiagnostics = null, accountId = null) {
  const status = $("research-artifact-status");
  const equityRows = view.equity?.rows ?? [];
  const fillRows = view.fills?.rows ?? [];
  const eventRows = view.events?.rows ?? [];
  const barRows = view.bars?.rows ?? [];
  const depthRows = view.depth?.rows ?? [];
  const isDepth = view.run_kind === "depth";
  const isMultiMarket = view.run_kind === "multi_market" || view.run_kind === "multi_market_depth" || view.run_kind === "multi_leg";
  const isMultiMarketDepth = view.run_kind === "multi_market_depth";
  const eventTraceAvailable = view.event_trace_available !== false;
  const totalReturn = equityReturnBps(equityRows);
  researchReportState.latestArtifactView = view;
  researchReportState.outcome = outcome;
  researchReportState.strategyDiagnostics = strategyDiagnostics;
  renderVerifiedArtifactMetrics(view);
  status.textContent = (accountId ? `账户 ${accountId} · ` : "") + "产物复核通过 · " + view.instrument + " · " + view.run_id
    + " · 净值 " + (view.equity?.total ?? 0) + " 点" + (view.equity?.truncated ? "（图表有界采样）" : "")
    + " · 成交 " + (view.fills?.total ?? 0) + " 笔（展示 " + fillRows.length + "）"
    + (eventTraceAvailable ? " · 事件 " + (view.events?.total ?? 0) + " 条（当前页 " + eventRows.length + " 条）" : " · 此运行未持久化事件轨迹")
    + " · 采样净值变化 " + (totalReturn === null ? "—" : researchBps(totalReturn));
  $("research-diagnostic-import").hidden = isDepth || Boolean(accountId);
  $("research-bars-heading").hidden = isDepth || isMultiMarket;
  $("research-bars-status").hidden = isDepth || isMultiMarket;
  $("research-bars-detail").hidden = isDepth || isMultiMarket;
  $("research-position-heading").hidden = isMultiMarket;
  $("research-position-status").hidden = isMultiMarket;
  $("research-position-plot").hidden = isMultiMarket;
  renderMultiMarketResearch(isMultiMarket ? view.markets : null);
  $("research-depth-detail").hidden = !isDepth;
  $("research-events-detail").hidden = !eventTraceAvailable;
  $("research-timeline-detail").hidden = !eventTraceAvailable || isDepth || view.run_kind === "multi_market_depth";
  const diagnosticBatch = view.market_strategy_diagnostics;
  if (Array.isArray(diagnosticBatch) && diagnosticBatch.length) {
    const selectorRow = $("research-diagnostic-select-row");
    const selector = $("research-diagnostic-select");
    selector.replaceChildren();
    for (const [index, attachment] of diagnosticBatch.entries()) {
      const report = attachment.report ?? {};
      const option = el("option", `${report.instrument ?? "标的"} · ${report.strategy_id ?? "策略"}`);
      option.value = String(index);
      selector.append(option);
    }
    selectorRow.hidden = false;
    selector.value = "0";
    selector.onchange = () => {
      const selected = diagnosticBatch[Number(selector.value)];
      if (selected) renderBoundStrategyDiagnostics(selected);
    };
    renderBoundStrategyDiagnostics(diagnosticBatch[0]);
  } else {
    $("research-diagnostic-select-row").hidden = true;
    $("research-diagnostic-select").replaceChildren();
    if (view.strategy_diagnostics) renderBoundStrategyDiagnostics(view.strategy_diagnostics);
  }
  const barStatus = view.bars?.source_status ?? "not_returned";
  $("research-bars-status").textContent = barStatus === "verified"
    ? `输入身份复核通过 · ${view.bars.total} 根 Bar · 展示 ${barRows.length}${view.bars.truncated ? "（等距采样）" : ""}；定点价格和成交量按原始整数展示。`
    : `未返回原始 Bar（${barStatus}）。只有位于 API 配置数据目录内并通过数据指纹复核的单 BarFrame 输入会展示。`;
  const depthStatus = view.depth?.source_status ?? "not_returned";
  $("research-depth-status").textContent = depthStatus === "verified"
    ? `输入身份复核通过 · ${view.depth.total} 份快照 · 展示 ${depthRows.length}${view.depth.truncated ? "（快照等距采样或盘口档位截断）" : ""}；每侧最多展示 20 档，价格和数量保留原始定点整数。`
    : `未返回原始盘口（${depthStatus}）。只有位于 API 配置数据目录内并通过输入指纹、清单和运行记录复核的深度帧会展示。`;
  if (researchReportState.comparisonCurves.length) {
    renderEquityComparisonPlot();
  } else {
    renderEquityDetailPlot(equityRows);
  }
  renderPositionDetailPlot(equityRows);
  renderResearchObjectTable("research-equity-table", equityRows, ["ts", "equity_raw", "position_raw"]);
  renderResearchObjectTable("research-bar-table", barRows, ["ts", "open_raw", "high_raw", "low_raw", "close_raw", "volume_raw"]);
  renderResearchObjectTable("research-depth-table", depthRows, ["ts", "sequence", "bids", "asks", "levels_truncated"]);
  renderResearchObjectTable("research-fill-table", fillRows);
  renderResearchEventPage(view);
}

function researchTimelineRows(eventRows) {
  return eventRows.map((event) => {
    const kind = event.kind;
    const eventKind = typeof kind === "string" ? kind : kind && typeof kind === "object" ? Object.keys(kind)[0] : "unknown";
    const eventPayload = typeof kind === "string" ? null : kind && typeof kind === "object" ? kind[eventKind] : null;
    const context = event.metadata?.context ?? {};
    const barContext = event.bar_context?.bar ?? null;
    const marketContext = event.market_context ?? null;
    const fact = event.research_fact ?? {};
    const cashflow = eventPayload?.cashflow ?? null;
    return {
      account_id: event.account_id ?? "—",
      seq: event.seq,
      ts: event.ts,
      priority: event.prio,
      event: eventKind,
      market_instrument: event.instrument ?? marketContext?.instrument ?? "—",
      linked_markets: Array.isArray(event.market_contexts)
        ? event.market_contexts.map((item) => item.instrument).join(", ")
        : "—",
      linked_market_bars: Array.isArray(event.market_contexts)
        ? event.market_contexts.map((item) => {
          const bar = item.bar_context?.bar;
          return `${item.instrument}: ${bar ? `${bar.ts} close=${bar.close_raw}` : item.bar_context_status}`;
        }).join("; ")
        : "—",
      bar_context_status: event.bar_context_status ?? "—",
      strategy_id: context.strategy_id || fact.strategy_id || "—",
      signal_id: context.signal_id || fact.signal_id || "—",
      order_id: eventPayload?.client_order_id ?? eventPayload?.order?.client_id ?? eventPayload?.fill?.order_id ?? fact.client_order_id ?? "—",
      signal_confidence: fact.confidence ?? "—",
      intent_count: fact.intent_count ?? "—",
      expires_at: fact.expires_at ?? "—",
      risk_instrument: fact.kind === "risk_decision" ? `${fact.instrument?.symbol ?? ""}.${fact.instrument?.venue ?? ""}` : "—",
      risk_outcome: fact.kind === "risk_decision" ? (fact.allowed ? "通过" : "拒绝") : "—",
      risk_rule_version: fact.rule_set_version ?? "—",
      risk_violations: fact.violations?.join("；") || "—",
      projected_position_raw: fact.projected_position_raw ?? "—",
      projected_margin_raw: fact.projected_margin_raw ?? "—",
      reference_price_raw: fact.reference_price_raw ?? "—",
      visible_bar_ts: barContext?.ts ?? "—",
      close_raw: barContext?.close_raw ?? "—",
      bar_relation: event.bar_context?.relation ?? "—",
      cashflow_kind: cashflow?.kind ?? "—",
      cashflow_currency: cashflow?.currency ?? "—",
      cashflow_amount_raw: cashflow?.amount?.raw ?? cashflow?.amount ?? "—",
    };
  });
}

function renderResearchEventPage(view) {
  const events = view.events ?? {};
  const eventRows = events.rows ?? [];
  const offset = Number(events.offset ?? 0);
  const total = Number(events.total ?? 0);
  const end = offset + eventRows.length;
  const first = eventRows.length ? offset + 1 : 0;
  $("research-event-page-status").textContent = total
    ? `事件 ${first}–${end} / ${total}；切页会重新校验完整事件日志。`
    : "没有事件记录";
  $("research-event-prev").disabled = offset <= 0;
  $("research-event-next").disabled = end >= total;
  renderResearchObjectTable("research-event-table", eventRows, ["seq", "ts", "prio", "kind", "metadata", "research_fact"]);
  if (view.run_kind === "depth" || view.run_kind === "multi_market_depth") {
    $("research-timeline-status").textContent = "深度事件按引擎顺序展示；Bar 行情因果关联不适用于盘口快照运行。";
  } else if (view.run_kind === "multi_market") {
    $("research-timeline-status").textContent = "组合事件按 strategy_id / instrument 关联到对应标的；Bar 上下文使用该标的已复核输入，并遵守事件优先级和策略决策严格前序 Bar 可见规则。无法唯一关联的账户级事件会明确标记 input_unavailable。";
    renderResearchTimeline(researchTimelineRows(eventRows));
  } else if (view.run_kind === "multi_leg") {
    $("research-timeline-status").textContent = "多腿事件关联到对应标的和已复核 Bar；双腿策略决策会同时展示两条腿的可见行情上下文，成交沿订单 ID 回溯到具体腿。";
    renderResearchTimeline(researchTimelineRows(eventRows));
  } else {
    $("research-timeline-status").textContent = "Bar 关联按事件优先级与时间限制在当时可见行情；原生 Bar 策略的决策意图和统一风控裁决作为独立事实显示，价格与仓位投影保留原始定点字符串。当前范围为这一页，旧版订单适配器可能没有策略决策事实。";
    renderResearchTimeline(researchTimelineRows(eventRows));
  }
}

async function loadResearchEventPage(offset) {
  const outcome = researchReportState.outcome;
  const current = researchReportState.latestArtifactView;
  if (!outcome || !current?.verification?.verified || !current.event_trace_available) return;
  const detailRequest = researchReportState.detailRequest;
  const pageRequest = ++researchReportState.eventPageRequest;
  $("research-event-page-status").textContent = "正在重放并校验完整事件日志…";
  $("research-event-prev").disabled = true;
  $("research-event-next").disabled = true;
  const request = { outcome, event_page: { offset, limit: 300 } };
  if (Array.isArray(researchReportState.strategyDiagnostics)) {
    request.market_strategy_diagnostics = researchReportState.strategyDiagnostics;
  } else if (researchReportState.strategyDiagnostics) {
    request.strategy_diagnostics = researchReportState.strategyDiagnostics;
  }
  try {
    const response = await postJson(API_PATHS.appResearchArtifacts, request);
    if (detailRequest !== researchReportState.detailRequest
      || pageRequest !== researchReportState.eventPageRequest) return;
    if (!response.ok) {
      $("research-event-page-status").textContent = "事件页读取失败："
        + (response.body?.message ?? response.body?.error ?? response.status);
      $("research-event-prev").disabled = Number(current.events?.offset ?? 0) <= 0;
      $("research-event-next").disabled = Number(current.events?.offset ?? 0)
        + (current.events?.rows?.length ?? 0) >= Number(current.events?.total ?? 0);
      return;
    }
    const next = response.body;
    if (!next?.verification?.verified || next.run_id !== current.run_id) {
      $("research-event-page-status").textContent = "事件页未通过产物身份或重放校验。";
      $("research-event-prev").disabled = Number(current.events?.offset ?? 0) <= 0;
      $("research-event-next").disabled = Number(current.events?.offset ?? 0)
        + (current.events?.rows?.length ?? 0) >= Number(current.events?.total ?? 0);
      return;
    }
    current.events = next.events;
    renderResearchEventPage(current);
  } catch (error) {
    if (detailRequest === researchReportState.detailRequest
      && pageRequest === researchReportState.eventPageRequest) {
      $("research-event-page-status").textContent = "事件页读取失败：" + error;
      $("research-event-prev").disabled = Number(current.events?.offset ?? 0) <= 0;
      $("research-event-next").disabled = Number(current.events?.offset ?? 0)
        + (current.events?.rows?.length ?? 0) >= Number(current.events?.total ?? 0);
    }
  }
}

function renderResearchTimeline(rows, { tableId = "research-timeline-table", filterId = "research-timeline-filter", countId = "research-timeline-count" } = {}) {
  const id = tableId;
  const filter = $(filterId);
  const count = $(countId);
  renderResearchObjectTable(id, rows, ["account_id", "seq", "ts", "priority", "event", "market_instrument", "linked_markets", "linked_market_bars", "strategy_id", "signal_id", "order_id", "cashflow_kind", "cashflow_currency", "cashflow_amount_raw", "signal_confidence", "intent_count", "expires_at", "risk_instrument", "risk_outcome", "risk_rule_version", "risk_violations", "projected_position_raw", "projected_margin_raw", "reference_price_raw", "visible_bar_ts", "close_raw", "bar_relation", "bar_context_status"]);
  const rendered = [...$(id).tBodies[0].rows];
  for (const row of rendered) row.dataset.search = row.textContent.toLocaleLowerCase();
  const applyFilter = () => {
    const query = filter.value.trim().toLocaleLowerCase();
    let visible = 0;
    for (const row of rendered) {
      row.hidden = !row.dataset.search.includes(query);
      if (!row.hidden) visible++;
    }
    count.textContent = `显示 ${visible} / ${rendered.length} 条事件`;
  };
  filter.oninput = applyFilter;
  applyFilter();
}

function stableResearchJson(value) {
  if (Array.isArray(value)) return `[${value.map(stableResearchJson).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableResearchJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function unifiedReportInput(view) {
  const markets = Array.isArray(view.markets) && view.markets.length
    ? view.markets.map((market) => ({
      instrument: market.instrument ?? null,
      data_fingerprint: market.data_fingerprint ?? null,
    })).sort((left, right) => String(left.instrument).localeCompare(String(right.instrument)))
    : [{
      instrument: view.instrument ?? null,
      data_fingerprint: view.verification?.data_fingerprint ?? null,
    }];
  if (markets.some((market) => !market.instrument || !market.data_fingerprint)) return null;
  return stableResearchJson(markets);
}

function unifiedReportRange(view) {
  const rows = view.equity?.rows ?? [];
  if (rows.length < 2) return null;
  const timestamp = (row) => row.ts ?? row.timestamp_ms ?? row.timestamp ?? null;
  const first = timestamp(rows[0]);
  const last = timestamp(rows[rows.length - 1]);
  if (first == null || last == null) return null;
  return stableResearchJson([first, last]);
}

function unifiedExecutionSignature(view) {
  const summary = view.summary ?? {};
  if (summary.execution_assumptions && typeof summary.execution_assumptions === "object") {
    if (Object.keys(summary.execution_assumptions).length) {
      return stableResearchJson(summary.execution_assumptions);
    }
  }
  const keys = [
    "fee_model", "fee_schedule", "fee_descriptor", "execution_cost_rules", "fill_model",
    "risk_rules", "margin_model", "market_specs", "market_spec", "execution_models",
    "execution_model", "execution_model_details", "order_policy",
  ];
  const assumptions = Object.fromEntries(keys
    .filter((key) => Object.prototype.hasOwnProperty.call(summary, key))
    .map((key) => [key, summary[key]]));
  return Object.keys(assumptions).length ? stableResearchJson(assumptions) : null;
}

function unifiedReportMetrics(view) {
  const summary = view.summary ?? {};
  const metrics = summary.metrics ?? summary;
  return {
    settlementCurrency: summary.settlement_currency ?? summary.account?.settlement_currency ?? summary.account?.currency ?? "未提供",
    returnBps: metrics.return_bps ?? metrics.weighted_return_bps ?? null,
    drawdownBps: metrics.max_drawdown_bps ?? null,
    feesRaw: metrics.fees_raw ?? null,
    costBps: metrics.cost_bps ?? null,
    finalEquityRaw: metrics.final_equity_raw ?? null,
    fills: view.fills?.total ?? metrics.fills ?? metrics.successful_fills ?? null,
  };
}

function researchTopologyLabel(runKind) {
  return ({
    depth: "单标的深度",
    multi_account: "多账户批次",
    multi_market: "共享账户多市场 Bar",
    multi_market_depth: "共享账户多市场深度",
    multi_leg: "共享账户多腿",
  })[runKind] ?? "单标的 Bar";
}

function addCurrentUnifiedResearchReport() {
  const row = researchReportState.selectedRow;
  const view = researchReportState.latestArtifactView;
  let outcome = row ? researchOutcomeForRow(row) : null;
  let accountId = null;
  const batch = researchReportState.accountBatch;
  if (batch?.selectedAccountId) {
    const account = batch.view.accounts?.find((item) => item.account_id === batch.selectedAccountId);
    const member = batch.outcome.runs?.find((item) => item.account_id === batch.selectedAccountId);
    if (account?.report?.verification?.verified && member?.outcome) {
      outcome = member.outcome;
      accountId = batch.selectedAccountId;
    }
  }
  if (!row || !view?.verification?.verified || view.run_id !== outcome?.run_id) {
    $("research-unified-status").textContent = "请先选中一条已完成产物复核的运行或账户。";
    return;
  }
  const key = `${view.run_id}\0${accountId ?? ""}`;
  if (researchReportState.unifiedReports.some((item) => item.key === key)) {
    $("research-unified-status").textContent = `${accountId ? `账户 ${accountId} · ` : ""}${view.run_id} 已在对照表中。`;
    return;
  }
  if (researchReportState.unifiedReports.length >= 8) {
    $("research-unified-status").textContent = "最多加入 8 条已复核结果。";
    return;
  }
  const inputMarkets = Array.isArray(view.markets) && view.markets.length
    ? view.markets.map((market) => ({
      instrument: market.instrument ?? null,
      data_fingerprint: market.data_fingerprint ?? null,
    })).sort((left, right) => String(left.instrument).localeCompare(String(right.instrument)))
    : [{
      instrument: view.instrument ?? null,
      data_fingerprint: view.verification.data_fingerprint ?? null,
    }];
  const executionSignature = unifiedExecutionSignature(view);
  researchReportState.unifiedReports.push({
    key,
    runId: view.run_id,
    accountId,
    topology: researchTopologyLabel(view.run_kind),
    rawTopology: view.run_kind ?? "single",
    instruments: inputMarkets.map((market) => market.instrument ?? "未提供").join(", "),
    inputDisplay: inputMarkets.map((market) => market.data_fingerprint ?? "未提供").join(", "),
    inputSignature: unifiedReportInput(view),
    rangeSignature: unifiedReportRange(view),
    rangeDisplay: unifiedReportRange(view) ?? "未提供",
    executionSignature,
    executionDisplay: executionSignature ? executionSignature.slice(0, 180) : "报告未提供完整假设",
    metrics: unifiedReportMetrics(view),
    verified: view.verification.verified,
    summary: view.summary ?? {},
    outcome,
    eventTraceAvailable: view.event_trace_available !== false,
  });
  researchReportState.unifiedEventRows = null;
  researchReportState.unifiedEventVerifiedCount = 0;
  $("research-unified-events-all").disabled = false;
  renderUnifiedResearchReports();
  renderUnifiedResearchEvents();
}

function renderUnifiedResearchReports() {
  const reports = researchReportState.unifiedReports;
  const baseline = reports[0] ?? null;
  const filter = $("research-unified-filter").value;
  const rows = reports.map((report, index) => {
    const sameInput = Boolean(baseline?.inputSignature && report.inputSignature
      && baseline.inputSignature === report.inputSignature);
    const sameRange = Boolean(baseline?.rangeSignature && report.rangeSignature
      && baseline.rangeSignature === report.rangeSignature);
    const sameExecution = Boolean(baseline?.executionSignature && report.executionSignature
      && baseline.executionSignature === report.executionSignature);
    const sameInputRange = sameInput && sameRange;
    const crossTopology = Boolean(baseline && report.rawTopology !== baseline.rawTopology);
    return {
      "报告 ID": report.runId,
      "回测拓扑": report.topology,
      "账户": report.accountId ?? "—",
      "结算币种": report.metrics.settlementCurrency,
      "标的": report.instruments,
      "数据指纹": report.inputDisplay,
      "样本区间": report.rangeDisplay,
      "收益": report.metrics.returnBps == null ? "未提供" : researchBps(report.metrics.returnBps),
      "最大回撤": report.metrics.drawdownBps == null ? "未提供" : researchBps(report.metrics.drawdownBps),
      "手续费 raw": report.metrics.feesRaw ?? "未提供",
      "单位成本": report.metrics.costBps == null ? "未提供" : researchBps(report.metrics.costBps),
      "成交数": report.metrics.fills ?? "未提供",
      "期末权益 raw": report.metrics.finalEquityRaw ?? "未提供",
      "执行假设": report.executionDisplay,
      "输入与区间匹配": index === 0 ? "基准" : sameInputRange ? "相同" : "不同或缺字段",
      "执行假设匹配": index === 0 ? "基准" : sameExecution ? "相同" : "不同或缺字段",
      "拓扑匹配": index === 0 ? "基准" : crossTopology ? "不同拓扑" : "相同拓扑",
      _baseline: index === 0,
      _sameInputRange: sameInputRange,
      _sameExecution: sameExecution,
      _crossTopology: crossTopology,
    };
  }).filter((row) => {
    if (filter === "same_input_range") return row._baseline || row._sameInputRange;
    if (filter === "same_execution") return row._baseline || row._sameExecution;
    if (filter === "same_input_range_execution") return row._baseline || (row._sameInputRange && row._sameExecution);
    if (filter === "cross_topology") return row._baseline || row._crossTopology;
    return true;
  }).map(({ _baseline, _sameInputRange, _sameExecution, _crossTopology, ...row }) => row);
  renderResearchObjectTable("research-unified-table", rows, [
    "报告 ID", "回测拓扑", "账户", "结算币种", "标的", "数据指纹", "样本区间",
    "收益", "最大回撤", "手续费 raw", "单位成本", "成交数", "期末权益 raw",
    "执行假设", "输入与区间匹配", "执行假设匹配", "拓扑匹配",
  ], 17);
  $("research-unified-status").textContent = reports.length
    ? `已复核 ${reports.length}/8 条；所有匹配标记均相对于首条基准。输入指纹、净值时间边界或执行假设缺失时标记为“不同或缺字段”。筛选结果 ${rows.length} 条。`
    : "尚未加入已复核结果。";
}

function clearUnifiedResearchReports() {
  researchReportState.unifiedEventGeneration += 1;
  researchReportState.unifiedEventLoading = false;
  researchReportState.unifiedEventPage = 0;
  researchReportState.unifiedEventRows = null;
  researchReportState.unifiedEventVerifiedCount = 0;
  $("research-unified-events-load").disabled = false;
  $("research-unified-events-all").disabled = false;
  $("research-unified-events-next").disabled = true;
  $("research-unified-events-page-prev").disabled = true;
  $("research-unified-events-page-next").disabled = true;
  researchReportState.unifiedReports = [];
  $("research-unified-filter").value = "all";
  renderUnifiedResearchReports();
  renderUnifiedResearchEvents();
}

async function loadUnifiedResearchEvents(initial = false, loadAll = false) {
  const reports = researchReportState.unifiedReports;
  const status = $("research-unified-events-status");
  if (!reports.length) {
    status.textContent = "先加入至少一条已复核报告。";
    return;
  }
  if (researchReportState.unifiedEventLoading) return;
  if (initial) {
    researchReportState.unifiedEventPage = 0;
    researchReportState.unifiedEventRows = null;
    researchReportState.unifiedEventVerifiedCount = 0;
    for (const report of reports) {
      report.unifiedEvents = report.eventTraceAvailable
        ? { rows: [], total: 0, nextOffset: 0, complete: false }
        : null;
    }
  }
  const cachedEvents = () => reports.reduce((sum, report) => sum + (report.unifiedEvents?.rows.length ?? 0), 0);
  const eventStreams = () => reports
    .filter((report) => report.eventTraceAvailable && report.unifiedEvents)
    .map((report) => ({
      key: report.key,
      runId: report.runId,
      rows: report.unifiedEvents.rows,
      complete: report.unifiedEvents.complete,
    }));
  const hasPending = reports.some((report) => report.eventTraceAvailable
    && report.outcome && !report.unifiedEvents?.complete);
  if (!hasPending) {
    const prefix = QxResearchComparison.verifiedUnifiedEventPrefix(eventStreams(), cachedEvents());
    if (prefix.valid) researchReportState.unifiedEventVerifiedCount = prefix.safeCount;
    status.textContent = reports.some((report) => !report.eventTraceAvailable)
      ? "所选报告均无可验证事件轨迹。"
      : "已加载所有所选报告的事件。";
    renderUnifiedResearchEvents();
    return;
  }
  researchReportState.unifiedEventLoading = true;
  const generation = ++researchReportState.unifiedEventGeneration;
  $("research-unified-events-load").disabled = true;
  $("research-unified-events-all").disabled = true;
  $("research-unified-events-next").disabled = true;
  const failedReports = new Set();
  const errors = [];
  let succeeded = 0;
  let requests = 0;
  const desiredCount = loadAll
    ? UNIFIED_EVENT_CACHE_LIMIT
    : Math.min(UNIFIED_EVENT_CACHE_LIMIT, researchReportState.unifiedEventVerifiedCount + UNIFIED_EVENT_PAGE_SIZE);
  const queryReport = async (report, limit) => {
    const current = report.unifiedEvents;
    const offset = Number(current?.nextOffset ?? 0);
    const reportError = (message) => Object.assign(new Error(`${report.runId}: ${message}`), { reportKey: report.key });
    const response = await postJson(API_PATHS.appResearchArtifacts, {
      outcome: report.outcome,
      event_page: { offset, limit },
    });
    if (!response.ok) {
      throw reportError(response.body?.message ?? response.body?.error ?? response.status);
    }
    const view = response.body;
    if (!view?.verification?.verified || view.run_id !== report.runId) {
      throw reportError("事件页未通过产物身份或重放校验");
    }
    const page = view.events;
    const pageOffset = Number(page?.offset);
    const pageTotal = Number(page?.total);
    if (!Array.isArray(page?.rows)
      || !Number.isSafeInteger(pageOffset)
      || !Number.isSafeInteger(pageTotal)
      || pageOffset !== offset
      || (offset > 0 && Number(current?.total) !== pageTotal)
      || page.rows.length > limit
      || pageOffset + page.rows.length > pageTotal
      || (page.rows.length === 0 && pageOffset < pageTotal)) {
      throw reportError("事件页游标、总量或返回行数不符合请求边界");
    }
    return { report, view };
  };
  let capped = false;
  while (true) {
    const loaded = cachedEvents();
    const prefix = QxResearchComparison.verifiedUnifiedEventPrefix(eventStreams(), desiredCount);
    if (!prefix.valid) {
      errors.push("事件源排序键无效或源内顺序不一致；拒绝生成全局分页视图");
      break;
    }
    researchReportState.unifiedEventVerifiedCount = prefix.safeCount;
    if (prefix.safe) break;
    if (loaded >= UNIFIED_EVENT_CACHE_LIMIT) {
      capped = reports.some((report) => report.eventTraceAvailable && !report.unifiedEvents?.complete);
      break;
    }
    const pendingKeys = new Set(prefix.needsMore);
    const pending = reports.filter((report) => pendingKeys.has(report.key)
      && report.eventTraceAvailable && report.outcome && !report.unifiedEvents?.complete
      && !failedReports.has(report.key));
    if (!pending.length) break;
    let passRemaining = UNIFIED_EVENT_CACHE_LIMIT - loaded;
    for (let index = 0; index < pending.length; index += 2) {
      if (passRemaining <= 0) break;
      const batchReports = pending.slice(index, index + 2);
      const limits = [];
      for (const report of batchReports) {
        if (passRemaining <= 0) break;
        const limit = Math.max(1, Math.min(UNIFIED_EVENT_PAGE_SIZE, passRemaining));
        limits.push({ report, limit });
        passRemaining -= limit;
      }
      if (!limits.length) break;
      const batch = await Promise.allSettled(limits.map(({ report, limit }) => queryReport(report, limit)));
      if (generation !== researchReportState.unifiedEventGeneration) return;
      requests += batch.length;
      for (const result of batch) {
        if (result.status === "rejected") {
          const message = String(result.reason?.message ?? result.reason);
          const failed = reports.find((report) => report.key === result.reason?.reportKey);
          if (failed) failedReports.add(failed.key);
          errors.push(message);
          continue;
        }
        const { report, view } = result.value;
        const page = view.events ?? { rows: [], total: 0 };
        const prior = report.unifiedEvents?.rows ?? [];
        const rows = [...prior, ...(page.rows ?? [])];
        const nextOffset = Number(report.unifiedEvents?.nextOffset ?? 0) + (page.rows?.length ?? 0);
        const total = Number(page.total ?? 0);
        report.unifiedEvents = {
          rows,
          total,
          nextOffset,
          complete: nextOffset >= total || (page.rows?.length ?? 0) === 0,
        };
        researchReportState.unifiedEventRows = null;
        succeeded += 1;
      }
      status.textContent = loadAll
        ? `正在复核并加载事件：${cachedEvents().toLocaleString()} / 最多 ${UNIFIED_EVENT_CACHE_LIMIT.toLocaleString()} 条…`
        : `已复核 ${succeeded} 份报告的事件页…`;
    }
    if (errors.length) break;
  }
  researchReportState.unifiedEventLoading = false;
  const finalPrefix = QxResearchComparison.verifiedUnifiedEventPrefix(eventStreams(), desiredCount);
  if (finalPrefix.valid) researchReportState.unifiedEventVerifiedCount = finalPrefix.safeCount;
  renderUnifiedResearchEvents();
  const loaded = cachedEvents();
  const total = reports.reduce((sum, report) => sum + (report.unifiedEvents?.total ?? 0), 0);
  const untracked = reports.filter((report) => !report.eventTraceAvailable).length;
  const hasMore = reports.some((report) => report.eventTraceAvailable
    && report.outcome && !report.unifiedEvents?.complete);
  status.textContent = errors.length
    ? `已复核 ${succeeded} 个事件页；缓存 ${loaded.toLocaleString()} / ${total.toLocaleString()} 条，仅显示可证明顺序的前 ${researchReportState.unifiedEventVerifiedCount.toLocaleString()} 条。失败：${errors.join("；")}`
    : capped
      ? `已达到浏览器缓存上限 ${loaded.toLocaleString()} 条；还有 ${Math.max(0, total - loaded).toLocaleString()} 条未加载。当前严格全局顺序已证明至 ${researchReportState.unifiedEventVerifiedCount.toLocaleString()} 条；请减少报告数量后继续${untracked ? `；${untracked} 份报告没有事件轨迹` : ""}。`
      : `已复核 ${requests} 个事件页；缓存 ${loaded.toLocaleString()} / ${total.toLocaleString()} 条，严格全局顺序已证明至 ${researchReportState.unifiedEventVerifiedCount.toLocaleString()} 条${untracked ? `；${untracked} 份报告没有事件轨迹` : ""}。`;
  $("research-unified-events-load").disabled = false;
  $("research-unified-events-all").disabled = !hasMore || loaded >= UNIFIED_EVENT_CACHE_LIMIT;
  $("research-unified-events-next").disabled = !reports.some((report) => report.eventTraceAvailable
    && report.unifiedEvents && !report.unifiedEvents.complete) || loaded >= UNIFIED_EVENT_CACHE_LIMIT;
}

function setUnifiedResearchEventFilterOptions(elementId, values) {
  const select = $(elementId);
  const selected = select.value;
  const unique = [...new Set(values
    .map((value) => String(value ?? ""))
    .filter((value) => value.length > 0))]
    .sort((left, right) => left < right ? -1 : left > right ? 1 : 0);
  const options = [new Option("全部", "")];
  for (const value of unique) options.push(new Option(value, value));
  select.replaceChildren(...options);
  if (unique.includes(selected)) select.value = selected;
}

function filterUnifiedResearchEvents(rows) {
  const selected = {
    report_id: $("research-unified-events-report").value,
    topology: $("research-unified-events-topology").value,
    account_id: $("research-unified-events-account").value,
    market_instrument: $("research-unified-events-instrument").value,
    event: $("research-unified-events-kind").value,
  };
  return QxResearchComparison.filterUnifiedEventRows(
    rows, selected, $("research-unified-events-filter").value,
  );
}

function exportUnifiedResearchEvents() {
  const rows = researchReportState.unifiedEventRows ?? [];
  const filtered = filterUnifiedResearchEvents(rows);
  if (!filtered.length) {
    $("research-unified-events-status").textContent = "当前筛选没有已复核、顺序可证明的事件可导出。";
    return;
  }
  const columns = [
    "report_id", "topology", "account_id", "seq", "ts", "priority", "event",
    "market_instrument", "linked_markets", "strategy_id", "signal_id", "order_id",
    "risk_outcome", "risk_violations", "cashflow_kind", "cashflow_currency",
    "cashflow_amount_raw", "bar_context_status", "visible_bar_ts", "close_raw",
  ];
  const csv = QxResearchComparison.unifiedEventCsv(filtered, columns);
  const blob = new Blob(["\uFEFF", csv], { type: "text/csv;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = `qianxing-research-events-${new Date().toISOString().slice(0, 10)}.csv`;
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
  $("research-unified-events-status").textContent = `已导出 ${filtered.length.toLocaleString()} 条已复核且顺序可证明的事件。`;
}

function renderUnifiedResearchEvents() {
  const reports = researchReportState.unifiedReports;
  let rows = researchReportState.unifiedEventRows;
  if (!rows) {
    const allReportsTracked = reports.every((report) => report.eventTraceAvailable);
    const streams = reports
      .filter((report) => report.eventTraceAvailable && report.unifiedEvents)
      .map((report) => ({
        key: report.key,
        runId: report.runId,
        rows: report.unifiedEvents.rows,
        complete: report.unifiedEvents.complete,
      }));
    const loaded = streams.reduce((sum, stream) => sum + stream.rows.length, 0);
    const prefix = allReportsTracked
      ? QxResearchComparison.verifiedUnifiedEventPrefix(streams, loaded)
      : { valid: false, rows: [] };
    const reportById = new Map(reports.map((report) => [report.runId, report]));
    const verifiedCount = prefix.valid
      ? Math.min(prefix.safeCount, researchReportState.unifiedEventVerifiedCount)
      : 0;
    rows = prefix.valid
      ? prefix.rows.slice(0, verifiedCount).flatMap((event) => {
        const report = reportById.get(event.report_id);
        if (!report) return [];
        const projected = researchTimelineRows([event])[0] ?? {};
        return [{
          ...projected,
          report_id: report.runId,
          topology: report.topology,
          account_id: report.accountId ?? projected.account_id,
        }];
      })
      : [];
    researchReportState.unifiedEventRows = rows;
  }
  setUnifiedResearchEventFilterOptions("research-unified-events-report", rows.map((row) => row.report_id));
  setUnifiedResearchEventFilterOptions("research-unified-events-topology", rows.map((row) => row.topology));
  setUnifiedResearchEventFilterOptions("research-unified-events-account", rows.map((row) => row.account_id));
  setUnifiedResearchEventFilterOptions("research-unified-events-instrument", rows.map((row) => row.market_instrument));
  setUnifiedResearchEventFilterOptions("research-unified-events-kind", rows.map((row) => row.event));
  const filtered = filterUnifiedResearchEvents(rows);
  const pageCount = Math.max(1, Math.ceil(filtered.length / UNIFIED_EVENT_PAGE_SIZE));
  researchReportState.unifiedEventPage = Math.min(researchReportState.unifiedEventPage, pageCount - 1);
  const pageStart = researchReportState.unifiedEventPage * UNIFIED_EVENT_PAGE_SIZE;
  const pageRows = filtered.slice(pageStart, pageStart + UNIFIED_EVENT_PAGE_SIZE);
  renderResearchObjectTable("research-unified-events-table", pageRows, [
    "report_id", "topology", "account_id", "seq", "ts", "priority", "event",
    "market_instrument", "linked_markets", "strategy_id", "signal_id", "order_id",
    "risk_outcome", "risk_violations", "cashflow_kind", "cashflow_currency",
    "cashflow_amount_raw", "bar_context_status", "visible_bar_ts", "close_raw",
  ], 21);
  const loaded = reports.reduce((sum, report) => sum + (report.unifiedEvents?.rows.length ?? 0), 0);
  const hasMore = reports.some((report) => report.eventTraceAvailable
    && report.outcome && !report.unifiedEvents?.complete);
  $("research-unified-events-page-status").textContent = filtered.length
    ? `筛选命中 ${filtered.length.toLocaleString()} / 已加载 ${loaded.toLocaleString()} 条 · 第 ${researchReportState.unifiedEventPage + 1}/${pageCount} 页`
    : `筛选命中 0 / 已加载 ${loaded.toLocaleString()} 条`;
  $("research-unified-events-page-prev").disabled = researchReportState.unifiedEventPage === 0;
  $("research-unified-events-page-next").disabled = researchReportState.unifiedEventPage + 1 >= pageCount;
  const untracked = reports.filter((report) => !report.eventTraceAvailable).length;
  $("research-unified-events-status").textContent = untracked
    ? `${untracked} 份所选报告没有事件轨迹，无法证明跨报告全局顺序；当前不显示合并时间线。`
    : `${pageRows.length} 条事件显示在当前页；仅展示所有事件源游标均已证明顺序的前缀，各报告序号相互独立。`;
  const canLoadMore = hasMore && loaded < UNIFIED_EVENT_CACHE_LIMIT;
  $("research-unified-events-all").disabled = researchReportState.unifiedEventLoading || !canLoadMore;
  const canContinue = reports.some((report) => report.eventTraceAvailable
    && report.unifiedEvents && !report.unifiedEvents.complete);
  $("research-unified-events-next").disabled = researchReportState.unifiedEventLoading
    || !canContinue || loaded >= UNIFIED_EVENT_CACHE_LIMIT;
}

function exportUnifiedResearchReports() {
  const table = $("research-unified-table");
  const rows = [...table.rows].map((row) => [...row.cells].map((cell) => cell.textContent ?? ""));
  if (rows.length <= 1) {
    $("research-unified-status").textContent = "当前筛选没有可导出的对照结果。";
    return;
  }
  const csv = rows.map((row) => row.map((value) => {
    const negativeNumeric = /^-\d+(?:\.\d+)?$/.test(value);
    const safe = /^[=+@\t\r]/.test(value) || (value.startsWith("-") && !negativeNumeric)
      ? `'${value}`
      : value;
    return `"${safe.replaceAll('"', '""')}"`;
  }).join(",")).join("\r\n");
  const blob = new Blob(["\uFEFF", csv], { type: "text/csv;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = `qianxing-research-comparison-${new Date().toISOString().slice(0, 10)}.csv`;
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
  $("research-unified-status").textContent = `已导出当前筛选的 ${rows.length - 1} 条结果。`;
}

function addCurrentResearchCurve() {
  const row = researchReportState.selectedRow;
  const view = researchReportState.latestArtifactView;
  let expectedOutcome = row ? researchOutcomeForRow(row) : null;
  let accountLabel = "";
  const batch = researchReportState.accountBatch;
  if (batch?.selectedAccountId) {
    const account = batch.view.accounts?.find((item) => item.account_id === batch.selectedAccountId);
    const member = batch.outcome.runs?.find((item) => item.account_id === batch.selectedAccountId);
    if (account?.report?.verification?.verified && member?.outcome) {
      expectedOutcome = member.outcome;
      accountLabel = `账户 ${batch.selectedAccountId} · `;
    }
  }
  if (!row || !view?.verification?.verified || view.run_id !== expectedOutcome?.run_id) {
    $("research-artifact-status").textContent = "请先选中并完成一条回测产物复核。";
    return;
  }
  const identity = view.verification.data_fingerprint;
  if (typeof identity !== "string" || !identity || typeof view.instrument !== "string" || !view.instrument) {
    $("research-equity-comparison-status").textContent = "无法加入对比：复核响应缺少标的或数据指纹。";
    return;
  }
  const first = researchReportState.comparisonCurves[0];
  const utcTimeAlignment = $("research-utc-time-alignment").checked;
  const comparisonMode = utcTimeAlignment
    ? "cross_input_utc"
    : $("research-cross-input-comparison").checked
      ? "cross_input_normalized"
      : "same_input";
  if (first && first.comparisonMode !== comparisonMode) {
    $("research-equity-comparison-status").textContent = "当前对比曲线使用另一种模式。请先清空，再按同输入或跨输入模式重新加入。";
    return;
  }
  if (first && comparisonMode === "same_input"
    && (first.instrument !== view.instrument || first.dataFingerprint !== identity)) {
    $("research-equity-comparison-status").textContent = "无法叠加：标的或输入数据指纹不同。请先清空对比，再选择相同输入的运行。";
    return;
  }
  if (researchReportState.comparisonCurves.some((curve) => curve.runId === view.run_id)) {
    $("research-equity-comparison-status").textContent = `${view.run_id} 已在对比图中。`;
    return;
  }
  if (researchReportState.comparisonCurves.length >= 8) {
    $("research-equity-comparison-status").textContent = "最多叠加 8 条曲线。";
    return;
  }
  const equityRows = view.equity?.rows ?? [];
  if (utcTimeAlignment && !QxResearchComparison.validateUtcEquityRows(equityRows).valid) {
    $("research-equity-comparison-status").textContent = "UTC 对齐需要至少两个严格递增的 Unix 毫秒权益时点；当前报告的时间列不满足条件。";
    return;
  }
  researchReportState.comparisonCurves.push({
    runId: view.run_id,
    label: accountLabel + (row.label || view.run_id),
    instrument: view.instrument,
    dataFingerprint: identity,
    settlementCurrency: view.summary?.settlement_currency ?? "",
    comparisonMode,
    visible: true,
    rows: equityRows,
  });
  renderEquityComparisonPlot();
}

function clearResearchComparison() {
  researchReportState.comparisonCurves = [];
  $("research-equity-legend").replaceChildren();
  $("research-equity-comparison-status").textContent = "对比曲线已清空。";
  const view = researchReportState.latestArtifactView;
  if (view?.verification?.verified) renderEquityDetailPlot(view.equity?.rows ?? []);
}

function renderEquityComparisonPlot() {
  const host = $("research-equity-plot");
  const legend = $("research-equity-legend");
  host.replaceChildren();
  legend.replaceChildren();
  const curves = researchReportState.comparisonCurves;
  if (!curves.length) return;
  const palette = ["var(--accent)", "var(--up)", "var(--down)", "#8b5cf6", "#0ea5e9", "#d97706", "#14b8a6", "#e11d48"];
  const normalized = [];
  for (const [index, curve] of curves.entries()) {
    const label = el("button", `${curve.label} · ${curve.settlementCurrency || "币种未知"} · ${curve.runId} · ${researchBps(equityReturnBps(curve.rows))}`, "research-curve-toggle");
    label.type = "button";
    label.dataset.runId = curve.runId;
    label.setAttribute("aria-pressed", String(curve.visible !== false));
    label.style.setProperty("--curve-color", palette[index % palette.length]);
    label.addEventListener("click", () => {
      curve.visible = curve.visible === false;
      renderEquityComparisonPlot();
    });
    legend.append(label);
    if (curve.rows.length < 2) continue;
    try {
      const start = BigInt(curve.rows[0].equity_raw);
      if (start === 0n) continue;
      const denominator = start < 0n ? -start : start;
      normalized.push({
        ...curve,
        paletteIndex: index,
        values: curve.rows.map((point) => Number((BigInt(point.equity_raw) - start) * 10000n / denominator)),
      });
    } catch {
      // One malformed curve must not corrupt the other replay-verified series.
    }
  }
  const visible = normalized.filter((curve) => curve.visible !== false);
  if (!visible.length) {
    host.textContent = normalized.length
      ? "所有曲线已隐藏，请点击图例恢复显示。"
      : "可对比净值点不足或无法转换。";
    return;
  }
  const comparisonMode = visible[0].comparisonMode;
  const utcAligned = comparisonMode === "cross_input_utc";
  let plotCurves = visible;
  let alignedTimestamps = null;
  let alignedStart = null;
  let alignedEnd = null;
  if (utcAligned) {
    const alignment = QxResearchComparison.alignUtcEquityCurves(visible);
    if (!alignment.ok) {
      const explanation = alignment.reason === "no-common-time-range"
        ? "所选曲线没有重叠的 UTC 时间区间。"
        : alignment.reason === "zero-or-missing-overlap-baseline"
          ? "共同区间起点的权益缺失或为零，无法计算归一化收益。"
          : alignment.reason === "need-more-curves"
            ? "UTC 对齐至少需要两条可见曲线。"
            : "权益点的时间戳或定点数值无效，无法进行 UTC 对齐。";
      host.textContent = explanation;
      $("research-equity-comparison-status").textContent = explanation;
      return;
    }
    const byRun = new Map(alignment.curves.map((curve) => [curve.runId, curve.values]));
    plotCurves = visible.map((curve) => ({ ...curve, values: byRun.get(curve.runId) ?? [] }));
    alignedTimestamps = alignment.timestamps.map((timestamp) => BigInt(timestamp));
    alignedStart = BigInt(alignment.startTimestamp);
    alignedEnd = BigInt(alignment.endTimestamp);
  }
  for (const curve of plotCurves) {
    const label = [...legend.querySelectorAll(".research-curve-toggle")]
      .find((item) => item.dataset.runId === curve.runId);
    if (label && utcAligned) {
      label.textContent = `${curve.label} · ${curve.settlementCurrency || "币种未知"} · ${curve.runId} · 共同区间 ${curve.values.at(-1)} bp`;
    }
  }
  const allValues = plotCurves.flatMap((curve) => curve.values);
  const minimum = Math.min(...allValues);
  const maximum = Math.max(...allValues);
  const span = Math.max(1, maximum - minimum);
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 800 220");
  svg.setAttribute("role", "img");
  const crossInput = comparisonMode !== "same_input";
  svg.setAttribute("aria-label", utcAligned
    ? "不同标的或输入的经复核权益收益对比，横轴为共同 UTC 时间区间，曲线按最近已观测权益前值保持"
    : crossInput
      ? "不同标的或输入的经复核归一化净值对比，横轴为各自样本进度，纵轴为相对首点收益基点"
      : "同一输入数据下经复核运行的归一化净值对比，纵轴为相对首点收益基点");
  for (const curve of plotCurves) {
    const points = curve.values.map((value, pointIndex) => {
      const x = utcAligned
        ? 28 + Number(alignedTimestamps[pointIndex] - alignedStart) / Number(alignedEnd - alignedStart) * 752
        : 28 + pointIndex / Math.max(1, curve.values.length - 1) * 752;
      return `${x},${185 - (value - minimum) / span * 145}`;
    }
    ).join(" ");
    const line = document.createElementNS("http://www.w3.org/2000/svg", "polyline");
    line.setAttribute("points", points);
    line.setAttribute("fill", "none");
    line.setAttribute("stroke", palette[curve.paletteIndex % palette.length]);
    line.setAttribute("stroke-width", "2");
    const title = document.createElementNS("http://www.w3.org/2000/svg", "title");
    title.textContent = `${curve.label} · ${curve.settlementCurrency || "结算币种未知"} · ${curve.runId} · ${researchBps(equityReturnBps(curve.rows))}`;
    line.append(title);
    svg.append(line);
  }
  for (const [y, label] of [[30, maximum + " bp"], [185, minimum + " bp"]]) {
    const text = document.createElementNS("http://www.w3.org/2000/svg", "text");
    text.setAttribute("x", "6");
    text.setAttribute("y", String(y));
    text.setAttribute("fill", "currentColor");
    text.setAttribute("font-size", "11");
    text.textContent = label;
    svg.append(text);
  }
  if (utcAligned) {
    for (const [x, timestamp, anchor] of [
      [28, alignedStart, "start"],
      [780, alignedEnd, "end"],
    ]) {
      const label = document.createElementNS("http://www.w3.org/2000/svg", "text");
      label.setAttribute("x", String(x));
      label.setAttribute("y", "211");
      label.setAttribute("text-anchor", anchor === "start" ? "start" : "end");
      label.setAttribute("fill", "currentColor");
      label.setAttribute("font-size", "9");
      label.textContent = new Date(Number(timestamp)).toISOString().replace("T", " ").slice(0, 16) + "Z";
      svg.append(label);
    }
  }
  host.append(svg);
  const first = plotCurves[0];
  if (utcAligned) {
    const currencies = [...new Set(plotCurves.map((curve) => curve.settlementCurrency || "未知"))].join(" / ");
    $("research-equity-comparison-status").textContent = `${plotCurves.length}/${curves.length} 条跨输入曲线可见 · 按共同 UTC 区间归一化，未更新时保持该曲线最近权益 · 结算币种：${currencies} · 未做汇率换算或组合净值合并。`;
  } else {
    $("research-equity-comparison-status").textContent = crossInput
      ? `${plotCurves.length}/${curves.length} 条跨输入曲线可见 · 按各自样本进度对齐并以首点归一化；未做自然时间或币种换算。`
      : `${plotCurves.length}/${curves.length} 条曲线可见 · ${first.instrument} · 相同数据指纹 ${first.dataFingerprint} · 每条曲线均按首个净值点归一化；纵轴范围随可见曲线调整。`;
  }
}

function renderBoundStrategyDiagnostics(attachment) {
  const report = attachment?.report;
  if (!attachment?.identity_bound || !report) return;
  $("research-diagnostic-result").hidden = false;
  const coverage = report.input_events !== undefined
    ? `${report.input_kind ?? "事件"} · ${report.input_events} 个输入事件`
    : `Bar · ${report.input_bars ?? "未知"} 根`;
  $("research-diagnostic-status").textContent = `身份与输入覆盖匹配（策略/标的/数据指纹 · ${coverage}）。诊断由本地工具计算，API 未执行策略或重算诊断。`;
  const lookahead = report.lookahead ?? {};
  const startup = report.startup ?? {};
  const startupTrajectory = startup.trajectory ?? {};
  const fieldList = (value) => Array.isArray(value) ? value.join(", ") || "—" : "—";
  const diagnosticEvents = report.input_events !== undefined;
  const inputUnit = diagnosticEvents ? "事件" : "Bar";
  const valueOf = (object, eventKey, barKey) => object?.[eventKey] ?? object?.[barKey] ?? "—";
  const pit = report.data_visibility;
  const pitFacts = pit?.affected_fact_counts ?? {};
  const pitChecks = pit?.affected_fact_decision_counts ?? {};
  const rows = [
    { 检查: "输入覆盖", 结果: coverage },
    { 检查: "未来数据敏感性", 结果: lookahead.status ?? "缺失", 检查点: lookahead.checks_run ?? 0, 抽样决策点: lookahead.decision_points_checked ?? "—", 决策变化: lookahead.flagged_checks ?? 0, 中间态变化: lookahead.intermediate_state_flagged_checks ?? 0, 已抽样: lookahead.checks_sampled ?? "—" },
    ...(lookahead.reason || lookahead.interpretation ? [{ 检查: "未来数据诊断边界", 结果: lookahead.reason ?? lookahead.interpretation }] : []),
    { 检查: "预热窗口敏感性", 结果: startup.status ?? "缺失", [`可见_${inputUnit}`]: startup.visible_events ?? startup.visible_bars ?? report.visible_events_at_as_of ?? report.visible_bars_at_as_of ?? "—", [`排除未来_${inputUnit}`]: startup.excluded_future_events ?? startup.excluded_future_bars ?? "—", 参考窗口完整: startup.reference_history_complete ?? "—", 决策变化窗口: fieldList(startup.changed_windows_events ?? startup.changed_windows_bars), 中间态变化窗口: fieldList(startup.intermediate_state_changed_windows_events ?? startup.intermediate_state_changed_windows_bars) },
    ...(startup.context_assumption ? [{ 检查: "预热诊断上下文", 结果: startup.context_assumption }] : []),
    { 检查: "预热历史轨迹", 结果: startupTrajectory.status ?? "未计算", 可比决策点: `${startupTrajectory.decision_points_checked ?? 0} / ${startupTrajectory.decision_points_eligible ?? startupTrajectory.decision_points_available ?? 0}`, 参考历史不足排除: startupTrajectory.decision_points_excluded_insufficient_reference_history ?? "—", 历史决策点: startupTrajectory.decision_points_available ?? 0, 已抽样: startupTrajectory.decision_points_sampled ?? "—", 决策变化窗口: fieldList(startupTrajectory.changed_windows_events ?? startupTrajectory.changed_windows_bars), 中间态变化窗口: fieldList(startupTrajectory.intermediate_state_changed_windows_events ?? startupTrajectory.intermediate_state_changed_windows_bars), 首次分歧时点: startupTrajectory.first_divergence?.decision_at ?? startupTrajectory.first_divergence?.decision_timestamp ?? "—" },
  ];
  if (pit) rows.push({
    检查: "PIT 可见性",
    结果: pit.status ?? "已检查",
    "晚可见事实/时点": `${pitFacts.future_value_unavailable ?? 0} / ${pitChecks.future_value_unavailable ?? 0}`,
    "事后修订事实/时点": `${pitFacts.revised_value_after_decision ?? 0} / ${pitChecks.revised_value_after_decision ?? 0}`,
    "发布时间未知事实/时点": `${pitFacts.availability_unknown ?? 0} / ${pitChecks.availability_unknown ?? 0}`,
    账本来源: pit.ledger_source ?? "—",
    账本指纹: pit.ledger_fingerprint ?? "—",
    "覆盖决策点": `${pit.decision_points_checked ?? 0} / ${pit.decision_points_available ?? 0}`,
  });
  const adjustmentChain = pit?.adjustment_chain;
  if (adjustmentChain) rows.push({
    检查: "公司行为与价格修订关联",
    结果: `${adjustmentChain.status ?? "未计算"} · 口径 ${adjustmentChain.price_adjustment_basis ?? "unknown"} · 修订 ${adjustmentChain.price_revision_count ?? 0} · 关联 ${adjustmentChain.candidate_count ?? 0} · 可能后复权前视 ${adjustmentChain.potential_back_adjustment_lookahead_count ?? 0} · 仅时间关联`,
  });
  renderResearchObjectTable("research-diagnostic-table", rows, ["检查", "结果"]);

  const trajectoryDivergences = Array.isArray(startupTrajectory.divergences)
    ? startupTrajectory.divergences
    : startupTrajectory.first_divergence ? [startupTrajectory.first_divergence] : [];
  const trajectoryLimit = 1000;
  const trajectorySamples = trajectoryDivergences.slice(0, trajectoryLimit).map((divergence, index) => {
    const decisionFields = divergence.changed_decision_fields ?? divergence.changed_fields;
    const stateFields = divergence.intermediate_state_changed_fields ?? divergence.changed_intermediate_fields;
    return {
      divergence,
      view: {
        轨迹检查点: index + 1,
        决策时点: divergence.decision_at ?? divergence.decision_timestamp ?? "—",
        对比窗口: `${valueOf(divergence, "requested_events", "requested_bars")} ${inputUnit} / ${valueOf(divergence, "reference_window_events", "reference_window_bars")} ${inputUnit}`,
        决策变化字段: fieldList(decisionFields),
        中间态变化字段: fieldList(stateFields),
      },
    };
  });
  const trajectoryStatus = $("research-diagnostic-trajectory-status");
  trajectoryStatus.textContent = trajectoryDivergences.length
    ? `记录 ${trajectoryDivergences.length} 个有分歧的检查点；${trajectoryDivergences.length > trajectorySamples.length ? `当前显示前 ${trajectorySamples.length} 个，完整记录仍在诊断 JSON 中。` : "时间点均来自本次有界轨迹抽样。"}`
    : Array.isArray(startupTrajectory.divergences)
      ? "本次轨迹没有观察到决策或已导出中间态分歧。"
      : "该诊断产物未包含逐时点分歧记录。";
  renderResearchObjectTable("research-diagnostic-trajectory-table", trajectorySamples.map((sample) => sample.view), [
    "轨迹检查点", "决策时点", "对比窗口", "决策变化字段", "中间态变化字段",
  ], 8, trajectoryLimit);
  const trajectoryFilter = $("research-diagnostic-trajectory-filter");
  const trajectoryCount = $("research-diagnostic-trajectory-count");
  const trajectoryDetail = $("research-diagnostic-trajectory-detail");
  trajectoryDetail.textContent = "选择一条轨迹分歧查看详情";
  trajectoryFilter.value = "";
  const trajectoryRendered = [...$("research-diagnostic-trajectory-table").tBodies[0].rows];
  for (const [index, row] of trajectoryRendered.entries()) {
    row.tabIndex = 0;
    row.setAttribute("role", "button");
    row.setAttribute("aria-label", `查看预热轨迹分歧 ${index + 1}`);
    row.dataset.search = row.textContent.toLocaleLowerCase();
    const showDetail = () => { trajectoryDetail.textContent = JSON.stringify(trajectorySamples[index].divergence, null, 2); };
    row.addEventListener("click", showDetail);
    row.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        showDetail();
      }
    });
  }
  const applyTrajectoryFilter = () => {
    const query = trajectoryFilter.value.trim().toLocaleLowerCase();
    let visible = 0;
    for (const row of trajectoryRendered) {
      row.hidden = !row.dataset.search.includes(query);
      if (!row.hidden) visible++;
    }
    trajectoryCount.textContent = `显示 ${visible} / ${trajectoryRendered.length} 个分歧检查点`;
  };
  trajectoryFilter.oninput = applyTrajectoryFilter;
  applyTrajectoryFilter();

  const evidence = [];
  for (const check of Array.isArray(lookahead.checks) ? lookahead.checks : []) {
    evidence.push({
      view: {
        类型: "未来扰动",
        决策时点: check.decision_as_of ?? "—",
        扰动: check.perturbation_profile ?? "—",
        决策变化: check.future_perturbation_changed_decision ?? "—",
        决策变化字段: fieldList(check.changed_decision_fields),
        中间态变化: check.intermediate_state_changed ?? "—",
        中间态变化字段: Array.isArray(check.intermediate_state_changed_fields) ? check.intermediate_state_changed_fields.join(", ") || "—" : "不可用",
      },
      detail: check,
    });
  }
  for (const window of Array.isArray(startup.windows) ? startup.windows : []) {
    evidence.push({
      view: {
        类型: "预热窗口",
        决策时点: startup.decision_as_of ?? "—",
        扰动: `${valueOf(window, "requested_events", "requested_bars")} ${inputUnit}（可见 ${valueOf(window, "available_events", "available_bars")}）`,
        决策变化: Array.isArray(window.changed_decision_fields) && window.changed_decision_fields.length > 0,
        决策变化字段: fieldList(window.changed_decision_fields),
        中间态变化: Array.isArray(window.intermediate_state_changed_fields) && window.intermediate_state_changed_fields.length > 0,
        中间态变化字段: Array.isArray(window.intermediate_state_changed_fields) ? fieldList(window.intermediate_state_changed_fields) : "不可用",
      },
      detail: window,
    });
  }
  const pitLabels = {
    future_value_unavailable: "决策时尚未发布",
    revised_value_after_decision: "决策后实值修订",
    availability_unknown: "发布时间未知",
  };
  const pitEvidence = Object.entries(pit?.examples ?? {}).flatMap(([status, examples]) =>
    (Array.isArray(examples) ? examples : []).map((sample) => {
      const availableAt = Array.isArray(sample.available_at)
        ? sample.available_at.map((value) => value == null ? "未知" : value).join(", ")
        : "—";
      return {
        status,
        sample,
        view: {
          风险: pitLabels[status] ?? status,
          事实ID: sample.fact_id ?? "—",
          实体: sample.entity_id ?? "—",
          字段: sample.field ?? "—",
          决策时点: sample.decision_at ?? "—",
          生效时点: sample.effective_at ?? "—",
          版本数: sample.version_count ?? "—",
          各版本发布时间: availableAt,
        },
      };
    })
  );
  for (const candidate of Array.isArray(adjustmentChain?.examples) ? adjustmentChain.examples : []) {
    pitEvidence.push({
      status: "adjustment_chain_correlation",
      sample: candidate,
      adjustment: true,
      view: {
        风险: candidate.potential_back_adjustment_lookahead
          ? "可能存在后复权前视（待核验）"
          : "价格修订与公司行为时间相关",
        事实ID: `${candidate.price_fact_id ?? "—"} ↔ ${candidate.action_fact_id ?? "—"}`,
        实体: candidate.price_entity_id ?? "—",
        字段: `${candidate.price_field ?? "—"} ↔ ${candidate.action_field ?? "—"}`,
        决策时点: `影响 ${candidate.affected_decision_points ?? 0} 点；复权前 ${candidate.pre_action_decision_points ?? 0} 点`,
        生效时点: candidate.action_effective_at ?? "—",
        版本数: candidate.price_revision ?? "—",
        各版本发布时间: candidate.action_availability_unknown
          ? "公司行为发布时间未知"
          : candidate.action_available_by_price_revision ? "修订时已知" : "修订时未知",
      },
    });
  }
  const pitSamples = pitEvidence.slice(0, 75);
  $("research-diagnostic-pit-status").textContent = pit
    ? `${pitEvidence.length} 条 PIT/复权链样本来自账本 ${pit.ledger_source ?? "（未命名）"}；指纹 ${pit.ledger_fingerprint ?? "缺失"}。复权口径 ${adjustmentChain?.price_adjustment_basis ?? "unknown"}；关联结果仅表示时间相关性，不证明因果。${pitEvidence.length > pitSamples.length ? `当前展示前 ${pitSamples.length} 条，机器报告保留完整结果。` : ""}`
    : "未附带 PIT 事实账本。";
  renderResearchObjectTable("research-diagnostic-pit-table", pitSamples.map((item) => item.view), ["风险", "事实ID", "实体", "字段", "决策时点", "生效时点", "版本数", "各版本发布时间"]);
  const pitFilter = $("research-diagnostic-pit-filter");
  const pitCount = $("research-diagnostic-pit-count");
  const pitDetail = $("research-diagnostic-pit-detail");
  pitDetail.textContent = "选择一条 PIT 样本查看账本版本";
  pitFilter.value = "";
  const pitRendered = [...$("research-diagnostic-pit-table").tBodies[0].rows];
  for (const [index, row] of pitRendered.entries()) {
    row.tabIndex = 0;
    row.setAttribute("role", "button");
    row.setAttribute("aria-label", `查看 PIT 事实样本 ${index + 1}`);
    row.dataset.search = row.textContent.toLocaleLowerCase();
    const showDetail = () => {
      pitDetail.textContent = JSON.stringify({
        visibility_status: pitSamples[index].status,
        ledger_source: pit?.ledger_source,
        ledger_fingerprint: pit?.ledger_fingerprint,
        ...(pitSamples[index].adjustment
          ? { adjustment_chain: pitSamples[index].sample, interpretation: adjustmentChain?.interpretation }
          : { fact: pitSamples[index].sample }),
      }, null, 2);
    };
    row.addEventListener("click", showDetail);
    row.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        showDetail();
      }
    });
  }
  const applyPitFilter = () => {
    const query = pitFilter.value.trim().toLocaleLowerCase();
    let visible = 0;
    for (const row of pitRendered) {
      row.hidden = !row.dataset.search.includes(query);
      if (!row.hidden) visible++;
    }
    pitCount.textContent = `显示 ${visible} / ${pitRendered.length} 条 PIT 样本`;
  };
  pitFilter.oninput = applyPitFilter;
  applyPitFilter();
  const sampleLimit = 200;
  const samples = evidence.slice(0, sampleLimit);
  $("research-diagnostic-evidence-status").textContent = evidence.length > sampleLimit
    ? `共 ${evidence.length} 个诊断样本，当前按输入顺序展示前 ${sampleLimit} 个；报告仍保留完整机器可读证据。`
    : `共 ${evidence.length} 个诊断样本。`;
  renderResearchObjectTable("research-diagnostic-evidence-table", samples.map((item) => item.view), ["类型", "决策时点", "扰动", "决策变化", "决策变化字段", "中间态变化", "中间态变化字段"]);
  const filter = $("research-diagnostic-evidence-filter");
  const count = $("research-diagnostic-evidence-count");
  const detail = $("research-diagnostic-evidence-detail");
  detail.textContent = "选择一条诊断样本查看详情";
  filter.value = "";
  const rendered = [...$("research-diagnostic-evidence-table").tBodies[0].rows];
  for (const [index, row] of rendered.entries()) {
    row.tabIndex = 0;
    row.setAttribute("role", "button");
    row.setAttribute("aria-label", `查看诊断样本 ${index + 1}`);
    row.dataset.search = row.textContent.toLocaleLowerCase();
    const showDetail = () => { detail.textContent = JSON.stringify(samples[index].detail, null, 2); };
    row.addEventListener("click", showDetail);
    row.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        showDetail();
      }
    });
  }
  const applyFilter = () => {
    const query = filter.value.trim().toLocaleLowerCase();
    let visible = 0;
    for (const row of rendered) {
      row.hidden = !row.dataset.search.includes(query);
      if (!row.hidden) visible++;
    }
    count.textContent = `显示 ${visible} / ${rendered.length} 条样本`;
  };
  filter.oninput = applyFilter;
  applyFilter();
}

async function attachStrategyDiagnostics() {
  const row = researchReportState.selectedRow;
  const file = $("research-diagnostic-file").files?.[0];
  const status = $("research-artifact-status");
  if (!row) {
    status.textContent = "请先选择一条可钻取的 Bar 或多市场 Bar 回测结果。";
    return;
  }
  if (!file) {
    status.textContent = "请选择本地策略偏差诊断 JSON 文件。";
    return;
  }
  if (file.size > 1024 * 1024) {
    status.textContent = "诊断 JSON 超过 1 MiB 上限。";
    return;
  }
  try {
    const diagnostic = JSON.parse(await file.text());
    const accountBatch = researchReportState.accountBatch;
    if (accountBatch) {
      if (!Array.isArray(diagnostic) || diagnostic.length === 0 || diagnostic.some((entry) =>
        !entry || typeof entry !== "object" || Array.isArray(entry)
        || typeof entry.account_id !== "string"
        || Boolean(entry.strategy_diagnostics) === Boolean(entry.market_strategy_diagnostics))) {
        throw new Error("多账户诊断文件须是 [{account_id, strategy_diagnostics}] 或 [{account_id, market_strategy_diagnostics}] 数组");
      }
      await loadResearchArtifactDetail(row, { account_strategy_diagnostics: diagnostic }, accountBatch.outcome);
      return;
    }
    await loadResearchArtifactDetail(row, diagnostic);
  } catch (error) {
    status.textContent = "诊断文件读取失败：" + error;
  }
}

function equityReturnBps(rows) {
  if (rows.length < 2) return null;
  try {
    const start = BigInt(rows[0].equity_raw);
    const end = BigInt(rows[rows.length - 1].equity_raw);
    if (start === 0n) return null;
    return Number((end - start) * 10000n / (start < 0n ? -start : start));
  } catch {
    return null;
  }
}

function renderResearchObjectTable(id, rows, preferred = [], maxColumns = 10, maxRows = 300) {
  const table = $(id);
  const head = table.tHead.rows[0];
  const body = table.tBodies[0];
  head.replaceChildren();
  body.replaceChildren();
  if (!rows.length) return;
  const keys = [...new Set([...preferred.filter((key) => rows.some((row) => key in row)), ...Object.keys(rows[0])])].slice(0, maxColumns);
  for (const key of keys) head.append(el("th", key));
  for (const row of rows.slice(0, maxRows)) {
    const tr = el("tr");
    for (const key of keys) tr.append(el("td", row[key] === undefined ? "—" : typeof row[key] === "object" ? JSON.stringify(row[key]) : row[key]));
    body.append(tr);
  }
}

function renderEquityDetailPlot(rows) {
  const host = $("research-equity-plot");
  host.replaceChildren();
  if (rows.length < 2) {
    host.textContent = "净值点不足，无法绘制曲线。";
    return;
  }
  let returns;
  try {
    const start = BigInt(rows[0].equity_raw);
    if (start === 0n) throw new Error("zero base");
    const denominator = start < 0n ? -start : start;
    returns = rows.map((row) => Number((BigInt(row.equity_raw) - start) * 10000n / denominator));
  } catch {
    host.textContent = "净值数据无法转换为精确整数。";
    return;
  }
  const minimum = Math.min(...returns);
  const maximum = Math.max(...returns);
  const span = Math.max(1, maximum - minimum);
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 800 220");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", "经复核的净值变化曲线，纵轴为相对首点收益基点");
  const points = returns.map((value, index) => (28 + index / (returns.length - 1) * 752) + "," + (185 - (value - minimum) / span * 145)).join(" ");
  const line = document.createElementNS("http://www.w3.org/2000/svg", "polyline");
  line.setAttribute("points", points);
  line.setAttribute("fill", "none");
  line.setAttribute("stroke", "var(--accent)");
  line.setAttribute("stroke-width", "2");
  svg.append(line);
  for (const [y, label] of [[30, maximum + " bp"], [185, minimum + " bp"]]) {
    const text = document.createElementNS("http://www.w3.org/2000/svg", "text");
    text.setAttribute("x", "6");
    text.setAttribute("y", String(y));
    text.setAttribute("fill", "currentColor");
    text.setAttribute("font-size", "11");
    text.textContent = label;
    svg.append(text);
  }
  host.append(svg);
}

function renderMarketContributionPlot(market) {
  const host = $("research-market-pnl-plot");
  host.replaceChildren();
  const rows = market.pnl_contribution?.rows ?? [];
  if (rows.length < 2) {
    host.textContent = "归因采样点不足，无法绘制曲线。";
    return;
  }
  let values;
  try {
    values = rows.map((row) => BigInt(row.pnl_contribution_raw));
  } catch {
    host.textContent = "盈亏贡献 raw 值无法解析为整数。";
    return;
  }
  let minimum = values[0];
  let maximum = values[0];
  for (const value of values) {
    if (value < minimum) minimum = value;
    if (value > maximum) maximum = value;
  }
  const span = maximum - minimum;
  const plotted = values.map((value) => span === 0n ? 0.5 : Number((value - minimum) * 1_000_000n / span) / 1_000_000);
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 800 220");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", `${market.instrument} 盈亏贡献曲线，纵轴为 ${market.pnl_contribution.currency} raw`);
  const line = document.createElementNS("http://www.w3.org/2000/svg", "polyline");
  line.setAttribute("points", plotted.map((value, index) => `${28 + index / (plotted.length - 1) * 752},${185 - value * 145}`).join(" "));
  line.setAttribute("fill", "none");
  line.setAttribute("stroke", "var(--accent)");
  line.setAttribute("stroke-width", "2");
  svg.append(line);
  const stride = Math.max(1, Math.ceil(rows.length / 200));
  for (let index = 0; index < rows.length; index += stride) {
    const circle = document.createElementNS("http://www.w3.org/2000/svg", "circle");
    circle.setAttribute("cx", String(28 + index / (rows.length - 1) * 752));
    circle.setAttribute("cy", String(185 - plotted[index] * 145));
    circle.setAttribute("r", "3");
    const title = document.createElementNS("http://www.w3.org/2000/svg", "title");
    title.textContent = `${rows[index].ts} · pnl_contribution_raw=${values[index]}`;
    circle.append(title);
    svg.append(circle);
  }
  for (const [y, label] of [[30, maximum.toString()], [185, minimum.toString()]]) {
    const text = document.createElementNS("http://www.w3.org/2000/svg", "text");
    text.setAttribute("x", "6");
    text.setAttribute("y", String(y));
    text.setAttribute("fill", "currentColor");
    text.setAttribute("font-size", "11");
    text.textContent = label;
    svg.append(text);
  }
  host.append(svg);
}

function renderPositionDetailPlot(rows) {
  const host = $("research-position-plot");
  const status = $("research-position-status");
  host.replaceChildren();
  const points = rows.filter((row) => row.position_raw !== undefined && row.position_raw !== null);
  if (points.length < 2) {
    status.textContent = "此运行没有足够的逐 Bar 持仓数据；若产物来自旧格式，该字段可能不存在。";
    host.textContent = "没有可绘制的持仓曲线";
    return;
  }
  let values;
  try {
    values = points.map((row) => BigInt(row.position_raw));
  } catch {
    status.textContent = "持仓 raw 值无法解析为整数。";
    host.textContent = "持仓数据格式无效";
    return;
  }
  let minimum = values[0];
  let maximum = values[0];
  for (const value of values) {
    if (value < minimum) minimum = value;
    if (value > maximum) maximum = value;
  }
  const span = maximum - minimum;
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 800 220");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", "经复核持仓变化曲线，明细保留原始定点整数");
  const plotted = values.map((value) => span === 0n ? 0.5 : Number((value - minimum) * 1_000_000n / span) / 1_000_000);
  const coordinates = plotted.map((value, index) => `${28 + index / (plotted.length - 1) * 752},${185 - value * 145}`);
  const line = document.createElementNS("http://www.w3.org/2000/svg", "polyline");
  line.setAttribute("points", coordinates.join(" "));
  line.setAttribute("fill", "none");
  line.setAttribute("stroke", "var(--up)");
  line.setAttribute("stroke-width", "2");
  svg.append(line);
  const stride = Math.max(1, Math.ceil(points.length / 200));
  for (let index = 0; index < points.length; index += stride) {
    const circle = document.createElementNS("http://www.w3.org/2000/svg", "circle");
    circle.setAttribute("cx", String(28 + index / (points.length - 1) * 752));
    circle.setAttribute("cy", String(185 - plotted[index] * 145));
    circle.setAttribute("r", "3");
    const title = document.createElementNS("http://www.w3.org/2000/svg", "title");
    title.textContent = `${points[index].ts} · position_raw=${values[index]}`;
    circle.append(title);
    svg.append(circle);
  }
  for (const [y, label] of [[30, maximum.toString()], [185, minimum.toString()]]) {
    const text = document.createElementNS("http://www.w3.org/2000/svg", "text");
    text.setAttribute("x", "6");
    text.setAttribute("y", String(y));
    text.setAttribute("fill", "currentColor");
    text.setAttribute("font-size", "11");
    text.textContent = label;
    svg.append(text);
  }
  host.append(svg);
  status.textContent = `范围 ${minimum} 至 ${maximum} raw · ${points.length} 个已复核采样点；悬停显示时间与精确持仓，表格保留完整 raw 字符串。`;
}

function reportRow(label, parameters, outcome, training, status, order, source) {
  return {
    label, parameters, outcome, training, status, order, source,
    returnBps: asNumber(outcome?.return_bps),
    drawdownBps: asNumber(outcome?.max_drawdown_bps),
    trainingBps: asNumber(training?.training_score_bps),
    robustnessBps: asNumber(training?.training_robustness_score_bps),
  };
}

function renderResearchReport(result, verification = null) {
  clearResearchReport();
  if (!result || typeof result !== "object") return;
  const metrics = $("research-report-metrics");
  const foldsBody = $("research-report-fold-table").tBodies[0];
  const costsBody = $("research-report-cost-table").tBodies[0];
  metrics.replaceChildren();
  foldsBody.replaceChildren();
  costsBody.replaceChildren();
  $("research-report-folds").hidden = true;
  $("research-report-costs").hidden = true;
  const identity = $("research-report-identity");
  const controls = $("research-report-controls");
  let rows = [];

  if (Array.isArray(result.candidates) && typeof result.experiment_id === "string") {
    researchReportState.kind = "experiment";
    identity.textContent = `实验 ${result.experiment_id} · 输入 ${result.input_data_fingerprint ?? "未提供"} · 规格 ${result.spec_fingerprint ?? "未提供"}。候选全样本收益仅供查看；选参应看训练窗与样本外。`;
    metrics.append(
      researchMetric("已完成候选", `${result.completed_candidates ?? "—"}/${result.total_candidates ?? "—"}`),
      researchMetric("成功 / 失败 / 剪枝", `${result.succeeded_candidates ?? "—"} / ${result.failed_candidates ?? "—"} / ${result.pruned_candidates ?? 0}`),
      researchMetric("样本外平均收益", researchBps(result.out_of_sample?.out_of_sample_return_bps)),
      researchMetric("训练到样本外衰减", researchBps(result.out_of_sample?.degradation_bps)),
    );
    if (result.search?.robustness_subwindows != null) {
      metrics.append(researchMetric(
        "训练稳健目标",
        `${result.search.robustness_subwindows} 个子窗的最差收益`,
      ));
    }
    if (result.search?.minimum_training_robustness_score_bps != null) {
      metrics.append(researchMetric(
        "训练稳健收益下限",
        researchBps(result.search.minimum_training_robustness_score_bps),
      ));
      const robustnessTrials = Array.isArray(result.search.fold_searches)
        ? result.search.fold_searches.flatMap((fold) => fold.trials ?? [])
        : result.search.trials ?? [];
      const eligibleRobustnessTrials = robustnessTrials.filter((trial) => (
        Number.isFinite(trial.training_robustness_score_bps)
        && trial.training_robustness_score_bps >= result.search.minimum_training_robustness_score_bps
      ));
      metrics.append(researchMetric(
        "训练稳健门达标轨迹",
        `${eligibleRobustnessTrials.length}/${robustnessTrials.length}`,
      ));
      if (robustnessTrials.length > 0 && eligibleRobustnessTrials.length === 0) {
        metrics.append(researchMetric("选参门状态", "没有候选达到训练稳健收益下限"));
      }
    }
    const crossFold = result.out_of_sample?.cross_fold_robustness;
    if (crossFold) {
      metrics.append(
        researchMetric("训练正收益折数", `${crossFold.positive_training_folds}/${crossFold.folds_evaluated}`),
        researchMetric("最差训练折", researchBps(crossFold.minimum_training_return_bps)),
        researchMetric("最差 OOS 折", researchBps(crossFold.minimum_out_of_sample_return_bps)),
        researchMetric("训练最差回撤", researchBps(crossFold.worst_training_drawdown_bps)),
      );
    }
    const trials = new Map((result.search?.trials ?? []).map((item) => [item.ordinal, item]));
    rows = result.candidates.map((candidate, index) => reportRow(
      `#${candidate.ordinal} ${candidate.outcome?.run_id ?? candidate.strategy_kind ?? "候选"}`,
      candidate.parameters ?? {}, candidate.outcome, trials.get(candidate.ordinal) ?? null,
      candidate.pruning ? `剪枝 · ${candidate.pruning.processed_bars} 根 Bar` : candidate.outcome ? "成功" : candidate.error ? "失败" : "未运行",
      index, candidate,
    ));
    const folds = result.out_of_sample?.folds ?? (result.search?.fold_searches ?? []).map((fold) => ({
      ...fold,
      in_sample_return_bps: null,
      out_of_sample_return_bps: null,
    }));
    const foldSearches = new Map((result.search?.fold_searches ?? []).map((item) => [item.fold, item]));
    if (folds.length) {
      $("research-report-folds").hidden = false;
      for (const fold of folds) {
        const foldNeighborhood = foldSearches.get(fold.fold)?.neighborhood;
        const neighborhoodText = foldNeighborhood
          ? `半径 ${foldNeighborhood.radius ?? 1} · ${foldNeighborhood.evaluated_neighbors}/${foldNeighborhood.possible_neighbors} · ${foldNeighborhood.assessment ?? "未评级"}`
          : "未计算";
        const tr = el("tr");
        tr.tabIndex = 0;
        for (const value of [
          fold.fold, (fold.train_range ?? []).join("–"), (fold.test_range ?? []).join("–"),
          fold.selected_ordinal == null ? "无合格候选" : `#${fold.selected_ordinal}`,
          researchBps(fold.in_sample_return_bps),
          researchBps(fold.selected_training_robustness_score_bps),
          researchBps(fold.out_of_sample_return_bps), neighborhoodText,
        ]) tr.append(el("td", value));
        const showDetail = () => showResearchDetail(fold);
        tr.addEventListener("click", showDetail);
        tr.addEventListener("keydown", (event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); showDetail(); } });
        foldsBody.append(tr);
      }
    }
    for (const baseline of result.baselines ?? []) {
      metrics.append(researchMetric(`基准 ${baseline.name}`, researchBps(baseline.return_bps)));
    }
    const neighborhood = result.search?.neighborhood;
    if (neighborhood) {
      metrics.append(researchMetric("参数邻域覆盖", `半径 ${neighborhood.radius ?? 1} · ${neighborhood.evaluated_neighbors ?? 0}/${neighborhood.possible_neighbors ?? 0}`));
      metrics.append(researchMetric("邻域判定（训练窗）", neighborhood.assessment ?? "未评估"));
    }
    if (result.search?.cost_robustness_passed != null) {
      metrics.append(researchMetric("成本稳健门（训练窗）", result.search.cost_robustness_passed ? "全部通过" : "存在未通过情景"));
    }
    if (result.search?.neighborhood_cost_robustness_passed != null) {
      metrics.append(researchMetric("邻域成本稳健门（训练窗）", result.search.neighborhood_cost_robustness_passed ? "全部通过" : "未通过或邻域覆盖不完整"));
    }
    const scenarios = result.cost_sensitivity ?? [];
    if (scenarios.length) {
      $("research-report-costs").hidden = false;
      for (const scenario of scenarios) {
        const tr = el("tr");
        tr.tabIndex = 0;
        const foldDeltas = Array.isArray(scenario.fold_deltas) ? scenario.fold_deltas : [];
        const splitDelta = (returnKey, drawdownKey) => {
          if (!foldDeltas.length) return "未提供";
          const average = (key) => Math.trunc(foldDeltas.reduce((sum, fold) => sum + Number(fold[key] ?? 0), 0) / foldDeltas.length);
          const prefix = foldDeltas.length > 1 ? `${foldDeltas.length} 折均值 · ` : "";
          return `${prefix}收益 ${researchBps(average(returnKey))} · 回撤 ${researchBps(average(drawdownKey))}`;
        };
        const costGate = scenario.max_training_return_degradation_bps == null
          ? "未设"
          : `${scenario.cost_robustness_passed ? "通过" : "未通过"}（≤ ${scenario.max_training_return_degradation_bps} bp）`;
        const neighbor = scenario.neighborhood_cost_robustness;
        const neighborhoodGate = scenario.max_neighborhood_training_return_degradation_bps == null
          ? "未设"
          : neighbor == null
            ? "证据缺失"
            : (neighbor.passed ? "通过" : "未通过") + "（覆盖 " + neighbor.evaluated_neighbors + "/" + neighbor.possible_neighbors + "，半径 " + neighbor.radius + "）";
        for (const value of [scenario.name, `#${scenario.selected_ordinal}`,
          splitDelta("training_return_delta_bps", "training_drawdown_delta_bps"),
          splitDelta("out_of_sample_return_delta_bps", "out_of_sample_drawdown_delta_bps"),
          `${researchBps(scenario.return_delta_bps)} / ${researchBps(scenario.drawdown_delta_bps)}`,
          costGate,
          neighborhoodGate,
          scenario.outcome ? "成功" : "失败"]) tr.append(el("td", value));
        const showDetail = () => showResearchDetail(scenario);
        tr.addEventListener("click", showDetail);
        tr.addEventListener("keydown", (event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); showDetail(); } });
        costsBody.append(tr);
      }
    }
  } else if (Array.isArray(result.runs) && typeof result.data_fingerprint === "string") {
    researchReportState.kind = "comparison";
    identity.textContent = `对比 ${result.instrument} · 数据 ${result.data_fingerprint}。排名来自 Rust 对比用例；该入口按调用方提交的指标排序，不复核回测产物。`;
    metrics.append(researchMetric("参与对比", result.runs.length));
    rows = result.runs.map((item, index) => reportRow(
      `#${item.rank} ${item.run?.run_id ?? "运行"}`, {}, item.run, null,
      "调用方指标", index, item,
    ));
  } else if (Array.isArray(result.runs) && typeof result.batch_id === "string" && result.summary) {
    researchReportState.kind = "multi_account";
    const summary = result.summary;
    identity.textContent = `多账户批次 ${result.batch_id} · ${result.settlement_currency ?? "结算币种未知"} · ${summary.complete ? "完整" : "部分结果"}。每个账户的回测产物会独立复核。`;
    metrics.append(
      researchMetric("账户成功 / 总数", `${summary.accounts_succeeded ?? 0} / ${summary.accounts_total ?? result.runs.length}`),
      researchMetric("失败账户", summary.accounts_failed ?? 0),
      researchMetric("加权收益", summary.weighted_return_bps == null ? "未计算" : researchBps(summary.weighted_return_bps)),
      researchMetric("成功成交", summary.successful_fills ?? 0),
    );
    rows = result.runs.map((item, index) => {
      const outcome = item.outcome ?? null;
      return reportRow(
        `${item.account_id ?? "账户未知"} · ${outcome?.run_id ?? "回测失败"}`,
        { account_id: item.account_id },
        outcome,
        null,
        outcome ? "等待产物复核" : "账户运行失败",
        index,
        result,
      );
    });
  } else if (typeof result.run_id === "string" && result.return_bps !== undefined) {
    researchReportState.kind = "backtest";
    identity.textContent = `运行 ${result.run_id} · ${result.instrument ?? "标的未知"} · 数据 ${result.data_fingerprint ?? "未提供"} · 结果 ${result.result_hash ?? "未提供"}`;
    metrics.append(
      researchMetric("全样本收益", researchBps(result.return_bps)),
      researchMetric("最大回撤", researchBps(result.max_drawdown_bps)),
      researchMetric("成交笔数", result.fills),
      researchMetric("产物复核", verification?.verified === true ? "已通过" : "未在此验证"),
    );
    rows = [reportRow(result.run_id, {}, result, null, verification?.verified === true ? "已复核" : "未复核", 0, result)];
  } else {
    return;
  }
  researchReportState.rows = rows;
  $("card-research-report").hidden = false;
  controls.hidden = rows.length <= 1;
  $("research-report-filter").value = "";
  $("research-report-sort").value = "server";
  renderResearchReportRows();
}

function renderResearchReportPlot(rows) {
  const host = $("research-report-plot");
  host.replaceChildren();
  const sort = $("research-report-sort").value;
  const useRobustness = researchReportState.kind === "experiment" && sort === "robustness";
  const useTraining = researchReportState.kind === "experiment" && !useRobustness && rows.some((row) => row.trainingBps !== null);
  const scored = rows.map((row) => ({ row, value: useRobustness ? row.robustnessBps : useTraining ? row.trainingBps : row.returnBps }))
    .filter((item) => item.value !== null);
  if (!scored.length) {
    $("research-report-plot-label").textContent = useRobustness
      ? "当前结果未配置训练子窗稳健性目标。"
      : "当前筛选没有可绘制的收益或训练分数。";
    host.textContent = "无可绘制的数值指标";
    return;
  }
  const stride = Math.max(1, Math.ceil(scored.length / 200));
  const points = scored.filter((_, index) => index % stride === 0);
  const values = points.map((point) => point.value);
  const minimum = Math.min(0, ...values);
  const maximum = Math.max(0, ...values);
  const span = Math.max(1, maximum - minimum);
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  const axisLabel = useRobustness ? "训练子窗最差收益" : useTraining ? "首训练窗收益" : "全样本收益";
  $("research-report-plot-label").textContent = `纵轴：${axisLabel}（基点），横轴按当前表格顺序；${scored.length} 个结果最多等距显示 200 个点。`;
  svg.setAttribute("viewBox", "0 0 800 180");
  svg.setAttribute("aria-label", `${axisLabel}，按当前筛选和排序显示 ${points.length} 个点`);
  const axis = document.createElementNS("http://www.w3.org/2000/svg", "line");
  const zeroY = 155 - ((0 - minimum) / span) * 130;
  for (const [name, value] of Object.entries({ x1: 28, x2: 780, y1: zeroY, y2: zeroY, stroke: "currentColor", "stroke-opacity": 0.4 })) axis.setAttribute(name, String(value));
  svg.append(axis);
  points.forEach(({ row, value }, index) => {
    const circle = document.createElementNS("http://www.w3.org/2000/svg", "circle");
    circle.setAttribute("cx", String(28 + (index / Math.max(1, points.length - 1)) * 752));
    circle.setAttribute("cy", String(155 - ((value - minimum) / span) * 130));
    circle.setAttribute("r", "4");
    circle.setAttribute("fill", value < 0 ? "var(--down)" : "var(--up)");
    const title = document.createElementNS("http://www.w3.org/2000/svg", "title");
    title.textContent = `${row.label}: ${researchBps(value)}`;
    circle.append(title);
    svg.append(circle);
  });
  host.append(svg);
}

function renderResearchReportRows() {
  const filter = $("research-report-filter").value.trim().toLocaleLowerCase();
  const sort = $("research-report-sort").value;
  const rows = researchReportState.rows.filter((row) =>
    `${row.label} ${JSON.stringify(row.parameters)} ${row.status}`.toLocaleLowerCase().includes(filter));
  const metric = sort === "return" ? "returnBps" : sort === "drawdown" ? "drawdownBps" : sort === "training" ? "trainingBps" : sort === "robustness" ? "robustnessBps" : null;
  if (metric) rows.sort((left, right) => {
    const a = left[metric];
    const b = right[metric];
    if (a === null) return b === null ? left.order - right.order : 1;
    if (b === null) return -1;
    return (sort === "drawdown" ? a - b : b - a) || left.order - right.order;
  });
  const body = $("research-report-table").tBodies[0];
  body.replaceChildren();
  for (const row of rows.slice(0, 500)) {
    const tr = el("tr");
    tr.tabIndex = 0;
    for (const value of [row.order + 1, row.label, JSON.stringify(row.parameters),
      researchBps(row.returnBps), researchBps(row.drawdownBps), researchBps(row.trainingBps),
      researchBps(row.robustnessBps), row.status]) {
      tr.append(el("td", value));
    }
    const showDetail = () => {
      researchReportState.selectedRow = row;
      showResearchDetail(row.source);
      void loadResearchArtifactDetail(row);
    };
    tr.addEventListener("click", showDetail);
    tr.addEventListener("keydown", (event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); showDetail(); } });
    body.append(tr);
  }
  $("research-report-count").textContent = `显示 ${Math.min(rows.length, 500)} / ${rows.length} 条匹配结果；图中最多采样 200 个点。筛选和排序只改变页面显示，不改变服务端结论。`;
  renderResearchReportPlot(rows);
}

// 其余研究工作流通过版本化 JSON 直接调用 qx-app 既有用例；浏览器不重算指标、撮合或排名。
async function runExtendedResearch() {
  const output = $("extended-research-output");
  const button = $("extended-research-run");
  if (!state.base || state.activeResearchRunId) return;
  clearResearchReport();
  button.disabled = true;
  output.textContent = "正在提交共享 Rust 研究用例…";
  try {
    const specText = $("extended-research-spec").value.trim();
    JSON.parse(specText);
    const routeKey = $("extended-research-route").value;
    const route = EXTENDED_RESEARCH_ROUTES[routeKey];
    if (!route) throw new Error("未登记的研究用例路由");
    const result = await postJsonText(route, specText);
    if (!result.ok) {
      output.textContent = JSON.stringify({ route, ...result }, null, 2);
      return;
    }
    const runId = result.body?.run_id;
    if (typeof runId !== "string" || !runId) {
      output.textContent = JSON.stringify({ route, ...result }, null, 2);
      renderResearchReport(result.body);
      return;
    }
    state.activeResearchRunId = runId;
    $("extended-research-query").disabled = false;
    $("extended-research-cancel").disabled = false;
    await pollExtendedResearchRun(runId, output);
  } catch (error) {
    output.textContent = error instanceof SyntaxError
      ? `请求规格 JSON 无效：${error.message}`
      : `研究请求失败：${error}`;
  } finally {
    button.disabled = Boolean(state.activeResearchRunId);
  }
}

async function pollExtendedResearchRun(runId, output) {
  const deadline = Date.now() + EXTENDED_RESEARCH_MAX_WAIT_MS;
  const statusPath = API_PATHS.appRunStatus.replace("{run_id}", encodeURIComponent(runId));
  while (state.activeResearchRunId === runId && Date.now() < deadline) {
    const response = await getJson(statusPath);
    if (!response.ok) {
      output.textContent = JSON.stringify({ run_id: runId, status_response: response }, null, 2);
      return;
    }
    const document = response.body || {};
    output.textContent = JSON.stringify({ run_id: runId, ...document }, null, 2);
    if (["succeeded", "failed", "cancelled"].includes(document.status)) {
      if (document.status === "succeeded") renderResearchReport(document.result);
      else clearResearchReport();
      state.activeResearchRunId = null;
      $("extended-research-run").disabled = false;
      $("extended-research-query").disabled = true;
      $("extended-research-cancel").disabled = true;
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, EXTENDED_RESEARCH_POLL_MS));
  }
  if (state.activeResearchRunId === runId) {
    output.textContent = `任务 ${runId} 仍在运行；轮询已到 30 分钟上限，可继续查询或取消。`;
  }
}

async function queryExtendedResearchRun() {
  const runId = state.activeResearchRunId;
  if (!runId) return;
  $("extended-research-query").disabled = true;
  try {
    await pollExtendedResearchRun(runId, $("extended-research-output"));
  } catch (error) {
    $("extended-research-output").textContent = `查询任务失败：${error}`;
  } finally {
    $("extended-research-query").disabled = !state.activeResearchRunId;
  }
}

async function cancelExtendedResearchRun() {
  const runId = state.activeResearchRunId;
  if (!runId) return;
  $("extended-research-cancel").disabled = true;
  try {
    const route = API_PATHS.appRunCancel.replace("{run_id}", encodeURIComponent(runId));
    const response = await postJson(route, {});
    $("extended-research-output").textContent = JSON.stringify({ run_id: runId, cancel: response }, null, 2);
  } catch (error) {
    $("extended-research-output").textContent = `取消请求失败：${error}`;
    $("extended-research-cancel").disabled = false;
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
  state.activeResearchRunId = null;
  if (state.timer) clearInterval(state.timer);
  state.timer = null;
  closeSocket();
  setConnState("未连接", "pill-idle");
  $("disconnect").disabled = true;
  $("ws-state").textContent = "WS 未连接";
  state.lastCommand = null;
  clearResearchReport();
  $("extended-research-run").disabled = false;
  $("extended-research-query").disabled = true;
  $("extended-research-cancel").disabled = true;
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
  $("research-strategy").addEventListener("change", (event) => {
    try {
      selectResearchStrategy(event.target.value);
    } catch (error) {
      $("research-output").textContent = `无法选择策略：${error.message}`;
    }
  });
  $("research-spec").addEventListener("input", syncResearchStrategySelector);
  syncResearchStrategySelector();
  $("extended-research-run").addEventListener("click", runExtendedResearch);
  $("extended-research-query").addEventListener("click", queryExtendedResearchRun);
  $("extended-research-cancel").addEventListener("click", cancelExtendedResearchRun);
  $("research-report-filter").addEventListener("input", renderResearchReportRows);
  $("research-report-sort").addEventListener("change", renderResearchReportRows);
  $("research-diagnostic-attach").addEventListener("click", attachStrategyDiagnostics);
  $("research-add-equity-curve").addEventListener("click", addCurrentResearchCurve);
  $("research-clear-equity-curves").addEventListener("click", clearResearchComparison);
  $("research-cross-input-comparison").addEventListener("change", (event) => {
    if (event.target.checked) $("research-utc-time-alignment").checked = false;
  });
  $("research-utc-time-alignment").addEventListener("change", (event) => {
    if (event.target.checked) $("research-cross-input-comparison").checked = false;
  });
  $("research-add-unified-report").addEventListener("click", addCurrentUnifiedResearchReport);
  $("research-export-unified-reports").addEventListener("click", exportUnifiedResearchReports);
  $("research-clear-unified-reports").addEventListener("click", clearUnifiedResearchReports);
  $("research-unified-filter").addEventListener("change", renderUnifiedResearchReports);
  $("research-unified-events-load").addEventListener("click", () => { void loadUnifiedResearchEvents(true); });
  $("research-unified-events-all").addEventListener("click", () => { void loadUnifiedResearchEvents(false, true); });
  $("research-unified-events-next").addEventListener("click", () => { void loadUnifiedResearchEvents(false); });
  $("research-unified-events-export").addEventListener("click", exportUnifiedResearchEvents);
  $("research-unified-events-filter").addEventListener("input", () => {
    researchReportState.unifiedEventPage = 0;
    renderUnifiedResearchEvents();
  });
  for (const id of [
    "research-unified-events-report",
    "research-unified-events-topology",
    "research-unified-events-account",
    "research-unified-events-instrument",
    "research-unified-events-kind",
  ]) {
    $(id).addEventListener("change", () => {
      researchReportState.unifiedEventPage = 0;
      renderUnifiedResearchEvents();
    });
  }
  $("research-unified-events-page-prev").addEventListener("click", () => {
    researchReportState.unifiedEventPage = Math.max(0, researchReportState.unifiedEventPage - 1);
    renderUnifiedResearchEvents();
  });
  $("research-unified-events-page-next").addEventListener("click", () => {
    researchReportState.unifiedEventPage += 1;
    renderUnifiedResearchEvents();
  });
  $("research-event-prev").addEventListener("click", () => {
    const events = researchReportState.latestArtifactView?.events;
    if (events) loadResearchEventPage(Math.max(0, Number(events.offset ?? 0) - 300));
  });
  $("research-event-next").addEventListener("click", () => {
    const events = researchReportState.latestArtifactView?.events;
    if (events) loadResearchEventPage(Number(events.offset ?? 0) + (events.rows?.length ?? 0));
  });
  $("research-account-event-prev").addEventListener("click", () => loadResearchAccountEventPage("previous"));
  $("research-account-event-next").addEventListener("click", () => loadResearchAccountEventPage("next"));
  $("research-account-event-instrument").addEventListener("change", resetResearchAccountEventInstrument);
  $("api-base").addEventListener("keydown", (event) => {
    if (event.key === "Enter") connect();
  });
});
