"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const source = fs.readFileSync(path.join(__dirname, "app.js"), "utf8");
const from = source.indexOf("async function refreshReadModels()");
const to = source.indexOf("// ---- 控制面（M3'）", from);
assert.notEqual(from, -1, "missing read-model refresh function");
assert.notEqual(to, -1, "missing control-plane boundary");

async function renderReadModels(reports) {
  let renderedRows = [];
  const paths = {
    ledger: "/account/ledger",
    reconcileReports: "/reconcile/reports",
    schedulerRuns: "/scheduler/runs",
    controlAudit: "/control/audit",
    strategyDeployments: "/strategies/deployments",
  };
  const context = {
    API_PATHS: paths,
    Date: { now: () => 2_000 },
    $(id) { return id; },
    rows(target, pairs) {
      if (target === "readmodels-rows") renderedRows = pairs;
    },
    tsText: (value) => String(value ?? "—"),
    errorOf: () => "fixture error",
    async getJson(url) {
      if (url === paths.reconcileReports) return { ok: true, status: 200, body: reports };
      return { ok: true, status: 200, body: [] };
    },
    advancePhases() {},
    renderControl() {},
  };
  vm.runInNewContext(
    `${source.slice(from, to)}\nglobalThis.refreshReadModels = refreshReadModels;`,
    context,
  );
  await context.refreshReadModels();
  return renderedRows;
}

test("missing reconciliation reports are shown as unknown, not clean", async () => {
  const rows = await renderReadModels([]);
  const summary = rows.find(([label]) => label === "对账详情");
  assert.match(summary[1], /没有持久化对账报告/);
  assert.match(summary[1], /不能据此认定账户无差异/);
  assert.doesNotMatch(summary[1], /0 项差异/);
});

test("reconciliation rows show observation age and exact discrepancy counts", async () => {
  const rows = await renderReadModels([
    {
      worker_id: "reconciler-clean",
      account_id: "main",
      venue_id: "OKX",
      observed_ts: 1_000,
      order_issues: [],
      balance_discrepancies: [],
    },
    {
      worker_id: "reconciler-issue",
      account_id: "hedge",
      venue_id: "BITGET",
      observed_ts: 1_500,
      order_issues: [{ kind: "unknown_order" }],
      balance_discrepancies: [{ asset: "USDT" }],
    },
    {
      worker_id: "reconciler-unknown",
      account_id: "main",
      venue_id: "BINANCE",
      observed_ts: 2_000,
    },
  ]);

  const clean = rows.find(([label]) => label.startsWith("对账 reconciler-clean"))[1];
  assert.match(clean, /报告记录 0 项差异/);
  assert.match(clean, /observed=1000; age=1000ms/);
  assert.match(clean, /order_issues=0; balance_discrepancies=0/);

  const issue = rows.find(([label]) => label.startsWith("对账 reconciler-issue"))[1];
  assert.match(issue, /存在差异/);
  assert.match(issue, /order_issues=1; balance_discrepancies=1/);

  const unknown = rows.find(([label]) => label.startsWith("对账 reconciler-unknown"))[1];
  assert.match(unknown, /报告字段不完整/);
  assert.match(unknown, /order_issues=未知; balance_discrepancies=未知/);
});
