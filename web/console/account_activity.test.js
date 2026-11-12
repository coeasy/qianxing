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
  let reportedRows = [];
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
      activityRequestSeq: 0,
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
    rows(_target, pairs) { reportedRows = pairs; },
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
          completeness: {
            event_context_coverage: {
              total_events: 4,
              with_correlation_id: 3,
              with_non_account_run_id: 2,
              with_strategy_id: 3,
              with_client_order_id: 2,
              with_exchange_order_id: 1,
            },
          },
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
  assert.match(reportedRows.find(([label]) => label === "事件上下文覆盖")[1], /3\/4 correlation/);
  assert.match(reportedRows.find(([label]) => label === "事件上下文覆盖")[1], /2\/4 非账户 run/);
});

test("a stale account activity response cannot replace the latest selection", async () => {
  const elements = new Map([
    ["account-activity-account-id", { value: "account-old" }],
    ["account-activity-venue-id", { value: "OKX" }],
    ["account-activity-client-order-id", { value: "" }],
    ["account-activity-exchange-order-id", { value: "" }],
    ["account-activity-run-id", { value: "" }],
    ["account-activity-strategy-id", { value: "" }],
    ["account-activity-meta", {}],
    ["account-activity-prev", { disabled: false }],
    ["account-activity-next", { disabled: false }],
  ]);
  const pending = [];
  const context = {
    state: {
      activityRequestSeq: 0,
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
    getJson() {
      return new Promise((resolve) => pending.push(resolve));
    },
  };
  vm.runInNewContext(
    `${source.slice(from, to)}\nglobalThis.refreshAccountActivity = refreshAccountActivity;`,
    context,
  );

  const oldRequest = context.refreshAccountActivity();
  elements.get("account-activity-account-id").value = "account-new";
  const newRequest = context.refreshAccountActivity();
  const result = (accountId) => ({
    ok: true,
    status: 200,
    body: {
      identity: { account_id: accountId, venue_id: "OKX" },
      events: { rows: [], available_total: accountId === "account-new" ? 1 : 99, has_older: false, has_newer: false },
      completeness: {},
    },
  });
  pending[1](result("account-new"));
  await newRequest;
  pending[0](result("account-old"));
  await oldRequest;
  assert.equal(context.state.activityPage.available_total, 1);
});

test("an order snapshot links to account events with its client ID and default account scope", () => {
  const focusStart = source.indexOf("function focusAccountActivityForOrder(");
  const focusEnd = source.indexOf("\nfunction rows(", focusStart);
  assert.notEqual(focusStart, -1, "missing order-to-account-activity action");
  assert.notEqual(focusEnd, -1, "missing order-to-account-activity boundary");
  const elements = new Map([
    ["account-activity-account-id", { value: "" }],
    ["account-activity-venue-id", { value: "" }],
    ["account-activity-client-order-id", { value: "" }],
    ["account-activity-exchange-order-id", { value: "" }],
    ["card-account-activity", { scrollIntoView() {} }],
    ["account-activity-load", { clicks: 0, click() { this.clicks += 1; } }],
  ]);
  const context = {
    state: { defaultAccountId: "account-1", defaultVenueId: "OKX" },
    $(id) { return elements.get(id); },
  };
  vm.runInNewContext(
    `${source.slice(focusStart, focusEnd)}\nglobalThis.focusAccountActivityForOrder = focusAccountActivityForOrder;`,
    context,
  );
  context.focusAccountActivityForOrder({ client_order_id: 823 });
  assert.equal(elements.get("account-activity-account-id").value, "account-1");
  assert.equal(elements.get("account-activity-venue-id").value, "OKX");
  assert.equal(elements.get("account-activity-client-order-id").value, "823");
  // The snapshot's order_id is local; don't guess that it's an exchange order ID.
  assert.equal(elements.get("account-activity-exchange-order-id").value, "");
  assert.equal(elements.get("account-activity-load").clicks, 1);
});
