const test = require("node:test");
const assert = require("node:assert/strict");
const {
  alignUtcEquityCurves,
  validateUtcEquityRows,
  verifiedUnifiedEventPrefix,
  filterUnifiedEventRows,
  unifiedEventCsv,
} = require("./research-comparison.js");

const t0 = 1_700_000_000_000n;
const row = (offset, equity) => ({ ts: (t0 + BigInt(offset)).toString(), equity_raw: String(equity) });

test("aligns returns on the shared UTC interval and holds the last observed equity", () => {
  const result = alignUtcEquityCurves([
    { runId: "a", rows: [row(0, 100), row(10, 110), row(20, 120)] },
    { runId: "b", rows: [row(10, 200), row(15, 180), row(20, 220)] },
  ]);
  assert.equal(result.ok, true);
  assert.equal(result.startTimestamp, String(t0 + 10n));
  assert.equal(result.endTimestamp, String(t0 + 20n));
  assert.deepEqual(result.timestamps, [String(t0 + 10n), String(t0 + 15n), String(t0 + 20n)]);
  assert.deepEqual(result.curves, [
    { runId: "a", values: [0, 0, 909] },
    { runId: "b", values: [0, -1000, 1000] },
  ]);
});

test("rejects curves without a common UTC interval", () => {
  const result = alignUtcEquityCurves([
    { runId: "a", rows: [row(0, 100), row(10, 101)] },
    { runId: "b", rows: [row(11, 200), row(20, 201)] },
  ]);
  assert.deepEqual(result, { ok: false, reason: "no-common-time-range" });
});

test("requires epoch-millisecond timestamps and strictly increasing samples", () => {
  assert.deepEqual(validateUtcEquityRows([{ ts: "0", equity_raw: "1" }, { ts: "1", equity_raw: "2" }]), { valid: false });
  assert.deepEqual(validateUtcEquityRows([row(0, 1), row(0, 2)]), { valid: false });
  assert.deepEqual(validateUtcEquityRows([row(0, 1), row(1, 2)]), { valid: true });
});

test("rejects a zero equity baseline instead of producing an invalid ratio", () => {
  const result = alignUtcEquityCurves([
    { runId: "a", rows: [row(0, 0), row(10, 1)] },
    { runId: "b", rows: [row(0, 10), row(10, 11)] },
  ]);
  assert.deepEqual(result, { ok: false, reason: "zero-or-missing-overlap-baseline" });
});

test("only exposes a cross-report event prefix proven by every source watermark", () => {
  const streams = [
    {
      key: "a",
      runId: "a",
      complete: false,
      rows: [
        { ts: "1", prio: 1, seq: 1 },
        { ts: "3", prio: 1, seq: 2 },
      ],
    },
    { key: "b", runId: "b", complete: false, rows: [{ ts: "2", prio: 1, seq: 1 }] },
  ];
  const partial = verifiedUnifiedEventPrefix(streams, 3);
  assert.equal(partial.valid, true);
  assert.equal(partial.safe, false);
  assert.equal(partial.safeCount, 2);
  assert.deepEqual(partial.needsMore, ["b"]);
  assert.deepEqual(partial.rows.map((event) => `${event.report_id}:${event.ts}`), ["a:1", "b:2"]);

  streams[1].rows.push({ ts: "4", prio: 1, seq: 2 });
  const completePrefix = verifiedUnifiedEventPrefix(streams, 3);
  assert.equal(completePrefix.safe, true);
  assert.equal(completePrefix.safeCount, 3);
  assert.deepEqual(completePrefix.rows.map((event) => `${event.report_id}:${event.ts}`), ["a:1", "b:2", "a:3"]);
});

test("keeps multi-account tie ordering and rejects unordered or unsafe source rows", () => {
  const tied = verifiedUnifiedEventPrefix([{
    key: "batch",
    runId: "batch",
    complete: true,
    rows: [
      { ts: "10", prio: 2, account_id: "account-a", seq: 9 },
      { ts: "10", prio: 2, account_id: "account-b", seq: 1 },
    ],
  }], 2);
  assert.equal(tied.safe, true);
  assert.deepEqual(tied.rows.map((event) => event.account_id), ["account-a", "account-b"]);
  assert.equal(verifiedUnifiedEventPrefix([{
    key: "bad",
    runId: "bad",
    complete: true,
    rows: [{ ts: "2", prio: 1, seq: 2 }, { ts: "1", prio: 1, seq: 1 }],
  }], 1).valid, false);
  assert.equal(verifiedUnifiedEventPrefix([{
    key: "unsafe",
    runId: "unsafe",
    complete: true,
    rows: [{ ts: "1", prio: 1, seq: Number.MAX_SAFE_INTEGER + 1 }],
  }], 1).valid, false);
  assert.equal(verifiedUnifiedEventPrefix([
    { key: "duplicate-a", runId: "same", complete: true, rows: [{ ts: "1", prio: 1, seq: 1 }] },
    { key: "duplicate-b", runId: "same", complete: true, rows: [{ ts: "2", prio: 1, seq: 2 }] },
  ], 2).valid, false);
  const unicodeTie = verifiedUnifiedEventPrefix([{
    key: "unicode",
    runId: "unicode",
    complete: true,
    rows: [
      { ts: "10", prio: 1, account_id: "\uE000", seq: 2 },
      { ts: "10", prio: 1, account_id: "\u{10000}", seq: 1 },
    ],
  }], 2);
  assert.equal(unicodeTie.valid, true);
  assert.deepEqual(unicodeTie.rows.map((event) => event.account_id), ["\uE000", "\u{10000}"]);
});

test("combines structured event filters with the text search", () => {
  const rows = [
    { report_id: "run-a", topology: "Bar", account_id: "account-1", market_instrument: "BTCUSDT", event: "Filled", order_id: "10" },
    { report_id: "run-b", topology: "Depth", account_id: "account-2", market_instrument: "ETHUSDT", event: "OrderAccepted", order_id: "11" },
    { report_id: "run-a", topology: "Bar", account_id: "account-1", market_instrument: "ETHUSDT", event: "Filled", order_id: "12" },
  ];
  const filtered = filterUnifiedEventRows(rows, {
    report_id: "run-a",
    topology: "Bar",
    account_id: "account-1",
    market_instrument: "ETHUSDT",
    event: "Filled",
  }, "12");
  assert.deepEqual(filtered.map((row) => row.order_id), ["12"]);
  assert.deepEqual(filterUnifiedEventRows(rows, { event: "Missing" }), []);
});

test("exports verified event rows as formula-safe CSV with exact strings", () => {
  const csv = unifiedEventCsv([{
    report_id: "=HYPERLINK(\"x\")",
    account_id: "account,one",
    seq: "9007199254740993",
    risk_violations: ["unsafe"],
    cashflow_amount_raw: "-100000000000000000001",
  }], ["report_id", "account_id", "seq", "risk_violations", "cashflow_amount_raw"]);
  assert.match(csv, /"'=HYPERLINK\(""x""\)"/);
  assert.match(csv, /"account,one"/);
  assert.match(csv, /"9007199254740993"/);
  assert.match(csv, /"\[""unsafe""\]"/);
  assert.match(csv, /"-100000000000000000001"/);
});
