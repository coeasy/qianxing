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

async function renderReadModels(reports, deployments = []) {
  let renderedRows = [];
  let renderedDeploymentRows = [];
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
      if (target === "strategy-deployments-rows") renderedDeploymentRows = pairs;
    },
    tsText: (value) => String(value ?? "—"),
    errorOf: () => "fixture error",
    async getJson(url) {
      if (url === paths.reconcileReports) return { ok: true, status: 200, body: reports };
      if (url === paths.strategyDeployments) return { ok: true, status: 200, body: deployments };
      if (url.startsWith(paths.controlAudit)) {
        return {
          ok: true,
          status: 200,
          body: {
            schema_version: 1,
            entries: [],
            next_cursor: null,
            has_more: false,
            snapshot_through_sequence: 0,
            oldest_available_sequence: null,
            retired_records: 0,
          },
        };
      }
      return { ok: true, status: 200, body: [] };
    },
    state: { lastCommand: null },
    URLSearchParams,
    advancePhases() {},
    renderControl() {},
  };
  vm.runInNewContext(
    `${source.slice(from, to)}\nglobalThis.refreshReadModels = refreshReadModels;`,
    context,
  );
  await context.refreshReadModels();
  renderedRows.deploymentRows = renderedDeploymentRows;
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

test("deployment rows keep missing trading authorization unevaluated", async () => {
  const rows = await renderReadModels([], [
    {
      worker_id: "legacy-worker",
      state: "ready",
      desired_revision: 3,
      desired_running: true,
      observed_at_ms: 1_000,
      blocking_reasons: [],
      reconciliation: {
        state: "unknown",
        fresh_report_count: 0,
        matching_report_count: 0,
        blocking_reasons: [],
      },
    },
    {
      worker_id: "blocked-worker",
      state: "blocked",
      desired_revision: 4,
      desired_running: true,
      observed_at_ms: 1_000,
      trading_enabled: false,
      blocking_reasons: ["operator authorization missing"],
    },
    {
      worker_id: "null-worker",
      state: "unknown",
      desired_revision: null,
      desired_running: null,
      observed_at_ms: null,
      trading_enabled: null,
      blocking_reasons: [],
      reconciliation: {
        state: "unknown",
        fresh_report_count: 0,
        matching_report_count: 0,
        freshest_age_ms: null,
        blocking_reasons: [],
      },
    },
  ]);
  const legacy = rows.deploymentRows.find(([label]) => label === "legacy-worker");
  const blocked = rows.deploymentRows.find(([label]) => label === "blocked-worker");
  const unknown = rows.deploymentRows.find(([label]) => label === "null-worker");
  assert.match(legacy[1], /trading_enabled=未评估/);
  assert.doesNotMatch(legacy[1], /undefined/);
  const oldReconciliation = rows.deploymentRows.find(([label]) => label === "legacy-worker / 账户对账");
  assert.match(oldReconciliation[1], /age=—/);
  assert.doesNotMatch(oldReconciliation[1], /undefinedms/);
  assert.match(blocked[1], /trading_enabled=false/);
  assert.match(unknown[1], /trading_enabled=未评估/);
  const nullReconciliation = rows.deploymentRows.find(([label]) => label === "null-worker / 账户对账");
  assert.match(nullReconciliation[1], /age=—/);
});
