"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const appSource = fs.readFileSync(path.join(__dirname, "app.js"), "utf8");
const from = appSource.indexOf("function orderGroupDetailNode(");
const to = appSource.indexOf("function rows(", from);
assert.notEqual(from, -1, "missing order group detail renderer");
assert.notEqual(to, -1, "missing renderer boundary");

const context = {
  document: {
    createElement(tagName) {
      return {
        tagName,
        children: [],
        append(...children) { this.children.push(...children); },
        textContent: "",
        className: "",
      };
    },
  },
};
vm.runInNewContext(
  `${appSource.slice(from, to)}\nglobalThis.renderOrderGroupDetails = orderGroupDetailNode;`,
  context,
);

test("order group detail disclosure renders the persisted snapshot as text", () => {
  const record = {
    intent: { group_id: "group-1", kind: "oco" },
    status: "ReconcileRequired",
    legs: [{ role: "stop_loss", venue_order_id: "remote-1" }],
    untrusted_venue_label: "<script>submit()</script>",
  };

  const details = context.renderOrderGroupDetails(record);
  assert.equal(details.tagName, "details");
  assert.equal(details.className, "order-group-details");
  assert.equal(details.children[0].tagName, "summary");
  assert.equal(details.children[0].textContent, "查看完整订单组事实");
  assert.equal(details.children[1].tagName, "pre");
  assert.equal(details.children[1].className, "order-group-facts");
  assert.equal(details.children[1].textContent, JSON.stringify(record, null, 2));
  assert.match(details.children[1].textContent, /<script>submit\(\)<\/script>/);
  assert.equal("innerHTML" in details.children[1], false);
});

test("order group table exposes group and child identities, including quote-budget orders", () => {
  const tbody = {
    children: [],
    replaceChildren(...children) { this.children = [...children]; },
    append(...children) { this.children.push(...children); },
  };
  const root = { querySelector: () => tbody };
  class Node {
    constructor(tagName) {
      this.tagName = tagName;
      this.children = [];
      this.textContent = "";
      this.listeners = {};
    }
    append(...children) { this.children.push(...children); }
    addEventListener(kind, callback) { this.listeners[kind] = callback; }
    click() { this.listeners.click?.(); }
  }
  const controls = {
    "account-activity-account-id": { value: "" },
    "account-activity-venue-id": { value: "" },
    "account-activity-client-order-id": { value: "" },
    "account-activity-exchange-order-id": { value: "" },
    "account-activity-load": { clicks: 0, click() { this.clicks += 1; } },
    "card-account-activity": { scrolls: [], scrollIntoView(options) { this.scrolls.push(options); } },
  };
  const context = {
    document: { createElement: (tagName) => new Node(tagName) },
    TABLE_SPECS: {
      "order-groups-table": {
        roster: "orderGroupRow",
        columns: ["intent", "status", "venue_group_id", "legs"],
      },
    },
    $: (id) => id === "order-groups-table" ? root : controls[id],
    errorOf: () => null,
    missingFields: () => [],
    shapeText: (value) => value.join(", "),
    el(tag, text, cls) {
      const node = new Node(tag);
      if (text !== undefined && text !== null) node.textContent = String(text);
      if (cls) node.className = cls;
      return node;
    },
    cellText: (_key, value) => value == null ? "—" : String(value),
    instrumentText: (value) => `${value.symbol}.${value.venue}`,
  };
  const detailsStart = appSource.indexOf("function orderGroupDetailNode(");
  const detailsEnd = appSource.indexOf("function rows(", detailsStart);
  const tableStart = appSource.indexOf("function fillTable(");
  const tableEnd = appSource.indexOf("async function refreshTables(", tableStart);
  vm.runInNewContext(
    `${appSource.slice(detailsStart, detailsEnd)}\n${appSource.slice(tableStart, tableEnd)}\nglobalThis.renderOrderGroupTable = fillTable;`,
    context,
  );

  const record = {
    intent: { group_id: "group-1", kind: "Oco", execution: "NativeRequired" },
    status: "ReconcileRequired",
    venue_group_id: "venue-group-1",
    legs: [{
      role: "StopLoss",
      venue_order_id: "venue-order-1",
      order: {
        client_id: 17,
        account_id: "main",
        instrument: { symbol: "BTCUSDT", venue: "BINANCE" },
        side: "Buy",
        status: "Unknown",
        qty: { raw: 0 },
        filled: { raw: 0 },
        exact_amounts: { quantity: "0", quote_budget: "100", filled: "0" },
      },
    }],
  };

  context.renderOrderGroupTable("order-groups-table", { ok: true, body: [record] });
  const row = tbody.children[0];
  assert.match(row.children[0].children[0].textContent, /group-1.*BTCUSDT\.BINANCE/);
  assert.equal(row.children[1].textContent, "ReconcileRequired");
  assert.equal(row.children[2].textContent, "venue-group-1");
  const legLine = row.children[3].children[0];
  assert.match(legLine.textContent, /client=17.*quote_budget 100.*venue_order=venue-order-1/);
  assert.equal(legLine.children[0].textContent, "查看账户事件");
  legLine.children[0].click();
  assert.equal(controls["account-activity-account-id"].value, "main");
  assert.equal(controls["account-activity-venue-id"].value, "BINANCE");
  assert.equal(controls["account-activity-client-order-id"].value, "17");
  assert.equal(controls["account-activity-exchange-order-id"].value, "venue-order-1");
  assert.equal(controls["account-activity-load"].clicks, 1);
  assert.deepEqual(
    JSON.parse(JSON.stringify(controls["card-account-activity"].scrolls)),
    [{ behavior: "smooth", block: "start" }],
  );
  assert.equal(row.children[0].children[1].children[1].textContent, JSON.stringify(record, null, 2));
});
