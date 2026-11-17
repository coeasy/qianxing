"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const appSource = fs.readFileSync(path.join(__dirname, "app.js"), "utf8");
const from = appSource.indexOf("function mergeRunEventPage(");
const to = appSource.indexOf("function renderExtendedResearchEvents(", from);
assert.notEqual(from, -1, "missing run-event page reducer");
assert.notEqual(to, -1, "missing run-event renderer boundary");

const context = {};
vm.runInNewContext(
  `${appSource.slice(from, to)}\nglobalThis.runEvents = { mergeRunEventPage, terminalRunEventError };`,
  context,
);

test("run-event pages append by stable run identity and contiguous sequence", () => {
  const first = context.runEvents.mergeRunEventPage("run-1", 0, [], [{
    run_id: "run-1", seq: 1, event_id: "run-1:1", kind: "Started", status: "Running",
  }]);
  const next = context.runEvents.mergeRunEventPage("run-1", first.cursor, first.events, [{
    run_id: "run-1", seq: 2, event_id: "run-1:2", kind: "Succeeded", status: "Succeeded",
  }]);
  assert.equal(next.cursor, 2);
  assert.deepEqual(JSON.parse(JSON.stringify(next.events.map((event) => event.event_id))), [
    "run-1:1", "run-1:2",
  ]);
});

test("run-event pages fail closed on sequence gaps, cross-run events, and unstable IDs", () => {
  assert.throws(() => context.runEvents.mergeRunEventPage("run-1", 0, [], [{
    run_id: "run-1", seq: 2, event_id: "run-1:2",
  }]), /序号断档/);
  assert.throws(() => context.runEvents.mergeRunEventPage("run-1", 0, [], [{
    run_id: "run-2", seq: 1, event_id: "run-1:1",
  }]), /run_id 不匹配/);
  assert.throws(() => context.runEvents.mergeRunEventPage("run-1", 0, [], [{
    run_id: "run-1", seq: 1, event_id: "other:1",
  }]), /身份不匹配/);
});

test("terminal run status requires its matching final lifecycle event", () => {
  const events = [{ kind: "succeeded", status: "succeeded" }];
  assert.equal(context.runEvents.terminalRunEventError("succeeded", events), null);
  assert.match(
    context.runEvents.terminalRunEventError("failed", events),
    /没有对应的终态事件/,
  );
  assert.equal(context.runEvents.terminalRunEventError("running", events), null);
});
