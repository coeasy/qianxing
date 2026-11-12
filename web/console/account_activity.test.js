"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const source = fs.readFileSync(path.join(__dirname, "app.js"), "utf8");
const from = source.indexOf("async function refreshAccountActivity()");
const to = source.indexOf("function fillTable(", from);
assert.notEqual(from, -1, "missing account-activity refresh function");
assert.notEqual(to, -1, "missing account-activity renderer boundary");

test("account activity filters run and strategy context through the HTTP query", async () => {
  let requestedPath;
  const elements = new Map([
    ["account-activity-account-id", { value: "account-1" }],
    ["account-activity-venue-id", { value: "OKX" }],
    ["account-activity-client-order-id", { value: "" }],
    ["account-activity-exchange-order-id", { value: "" }],
    ["account-activity-run-id", { value: "run/4" }],
    ["account-activity-strategy-id", { value: "mean reversion" }],
    ["account-activity-meta", {}],
    ["account-activity-prev", { disabled: false }],
    ["account-activity-next", { disabled: false }],
  ]);
  const context = {
    state: {
      activityAccountId: null,
      activityVenueId: null,
      activityClientOrderId: null,
      activityExchangeOrderId: null,
      activityRunId: null,
      activityStrategyId: null,
      activityAfter: null,
      activityBefore: null,
      activitySelectionDirty: false,
    },
    API_PATHS: { accountActivity: "/account/activity" },
    URLSearchParams,
    $(id) { return elements.get(id); },
    rows() {},
    fillTable() {},
    tsText() { return "—"; },
    cellText() { return "—"; },
    async getJson(pathname) {
      requestedPath = pathname;
      return {
        status: 200,
        ok: true,
        body: {
          identity: { account_id: "account-1", venue_id: "OKX" },
          events: { rows: [], available_total: 0, has_older: false, has_newer: false },
          completeness: {},
        },
      };
    },
  };
  vm.runInNewContext(
    `${source.slice(from, to)}\nglobalThis.refreshAccountActivity = refreshAccountActivity;`,
    context,
  );
  await context.refreshAccountActivity();
  const query = new URLSearchParams(requestedPath.split("?")[1]);
  assert.equal(query.get("account_id"), "account-1");
  assert.equal(query.get("venue_id"), "OKX");
  assert.equal(query.get("run_id"), "run/4");
  assert.equal(query.get("strategy_id"), "mean reversion");
  assert.equal(context.state.activityPage.available_total, 0);
});
