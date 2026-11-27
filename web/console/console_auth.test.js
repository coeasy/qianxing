"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const appSource = fs.readFileSync(path.join(__dirname, "app.js"), "utf8");

function sourceBetween(start, end) {
  const from = appSource.indexOf(start);
  const to = appSource.indexOf(end, from + start.length);
  assert.notEqual(from, -1, `missing app.js section: ${start}`);
  assert.notEqual(to, -1, `missing end marker: ${end}`);
  return appSource.slice(from, to);
}

function loadAuthFunctions(cookie) {
  const calls = [];
  const cookieName = appSource.match(/const CSRF_COOKIE_NAME = "([^"]+)"/);
  const headerName = appSource.match(/const CSRF_HEADER_NAME = "([^"]+)"/);
  assert.ok(cookieName, "missing CSRF cookie name in app.js");
  assert.ok(headerName, "missing CSRF header name in app.js");
  assert.equal(cookieName[1], "qx_console_csrf");
  assert.equal(headerName[1].toLowerCase(), "x-qx-csrf");
  const context = {
    document: { cookie },
    state: { base: "http://127.0.0.1:18082" },
    CSRF_COOKIE_NAME: cookieName[1],
    CSRF_HEADER_NAME: headerName[1],
    fetchWithTimeout: async (url, init) => {
      calls.push({ url, init });
      return { status: 202, ok: true, text: async () => "{}" };
    },
    readJson: async (response) => JSON.parse(await response.text()),
  };
  const csrf = sourceBetween("function csrfToken() {", "async function readJson(");
  const post = sourceBetween("async function postJsonText(", "function newRequestId(");
  vm.runInNewContext(`${csrf}\n${post}\nglobalThis.auth = { csrfToken, postJsonText };`, context);
  return { auth: context.auth, calls };
}

test("packaged console reads its CSRF cookie and sends it with same-origin writes", async () => {
  const { auth, calls } = loadAuthFunctions("other=1; qx_console_csrf=csrf%2Bvalue");
  assert.equal(auth.csrfToken(), "csrf+value");

  const result = await auth.postJsonText("/control/commands", '{"request_id":"r1"}');
  assert.deepEqual(JSON.parse(JSON.stringify(result)), {
    status: 202,
    ok: true,
    body: {},
  });
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, "http://127.0.0.1:18082/control/commands");
  assert.equal(calls[0].init.method, "POST");
  assert.equal(calls[0].init.credentials, "same-origin");
  assert.equal(calls[0].init.headers["X-QX-CSRF"], "csrf+value");
  assert.equal(calls[0].init.body, '{"request_id":"r1"}');
});

test("packaged console does not invent a CSRF token when the cookie is absent", async () => {
  const { auth, calls } = loadAuthFunctions("other=1");
  assert.equal(auth.csrfToken(), "");
  await auth.postJsonText("/control/commands", "{}");
  assert.equal(calls[0].init.headers["X-QX-CSRF"], undefined);
});

function loadSampleInstaller({ connected, viaBff, response }) {
  const calls = [];
  const button = { disabled: false };
  const output = { textContent: "" };
  const context = {
    state: { connected, viaBff },
    $(id) {
      return id === "install-sample" ? button : output;
    },
    postJsonText: async (path, body) => {
      calls.push({ path, body });
      return response;
    },
  };
  const installer = sourceBetween("async function installConsoleSample() {", "function newRequestId(");
  vm.runInNewContext(`${installer}\nglobalThis.install = installConsoleSample;`, context);
  return { install: context.install, calls, button, output };
}

test("one-click sample installation uses the authenticated Console BFF and reports its outcome", async () => {
  const sample = loadSampleInstaller({
    connected: true,
    viaBff: true,
    response: {
      status: 201,
      ok: true,
      body: { status: "installed", bytes: 4096, sha256: "a".repeat(64) },
    },
  });
  assert.equal(await sample.install(), true);
  assert.deepEqual(sample.calls, [{ path: "/console/sample-data", body: "" }]);
  assert.match(sample.output.textContent, /教学数据已安装/);
  assert.equal(sample.button.disabled, false);
});

test("static Console never sends the BFF-only sample installation write", async () => {
  const sample = loadSampleInstaller({ connected: true, viaBff: false, response: null });
  assert.equal(await sample.install(), false);
  assert.equal(sample.calls.length, 0);
  assert.match(sample.output.textContent, /仅由同源 Console 提供/);
});

function loadConsoleQuickstart({ connected, viaBff, spec, installResult = true, researchResult = true }) {
  const nodes = {
    "run-console-quickstart": { disabled: false },
    "console-quickstart-status": { textContent: "" },
    "research-spec": { value: spec },
  };
  const calls = { installs: 0, research: [] };
  const context = {
    state: { connected, viaBff },
    $(id) {
      assert.ok(nodes[id], `unexpected Console quickstart element: ${id}`);
      return nodes[id];
    },
    installConsoleSample: async () => {
      calls.installs += 1;
      return installResult;
    },
    runResearch: async (validateOnly) => {
      calls.research.push(validateOnly);
      return researchResult;
    },
  };
  const quickstart = sourceBetween("async function runConsoleQuickstart() {", "function newRequestId(");
  vm.runInNewContext(`${quickstart}\nglobalThis.run = runConsoleQuickstart;`, context);
  return { run: context.run, calls, nodes };
}

test("one-click Console quickstart installs the sample then uses the verified research flow", async () => {
  const quickstart = loadConsoleQuickstart({
    connected: true,
    viaBff: true,
    spec: '{"schema_version":1,"bars_path":"sample.json"}',
  });
  assert.equal(await quickstart.run(), true);
  assert.equal(quickstart.calls.installs, 1);
  assert.deepEqual(quickstart.calls.research, [false]);
  assert.match(quickstart.nodes["console-quickstart-status"].textContent, /回测产物复核通过/);
  assert.equal(quickstart.nodes["run-console-quickstart"].disabled, false);
});

test("one-click Console quickstart preserves manual flow for static pages and custom data", async () => {
  const staticPage = loadConsoleQuickstart({
    connected: true,
    viaBff: false,
    spec: '{"schema_version":1,"bars_path":"sample.json"}',
  });
  assert.equal(await staticPage.run(), false);
  assert.equal(staticPage.calls.installs, 0);
  assert.match(staticPage.nodes["console-quickstart-status"].textContent, /静态页面/);

  const customData = loadConsoleQuickstart({
    connected: true,
    viaBff: true,
    spec: '{"schema_version":1,"bars_path":"custom.json"}',
  });
  assert.equal(await customData.run(), false);
  assert.equal(customData.calls.installs, 0);
  assert.equal(customData.calls.research.length, 0);
  assert.match(customData.nodes["console-quickstart-status"].textContent, /bars_path.*sample\.json/);
});

test("one-click Console quickstart stops when safe sample installation or verification fails", async () => {
  const installConflict = loadConsoleQuickstart({
    connected: true,
    viaBff: true,
    spec: '{"schema_version":1,"bars_path":"sample.json"}',
    installResult: false,
  });
  assert.equal(await installConflict.run(), false);
  assert.equal(installConflict.calls.research.length, 0);

  const failedResearch = loadConsoleQuickstart({
    connected: true,
    viaBff: true,
    spec: '{"schema_version":1,"bars_path":"sample.json"}',
    researchResult: false,
  });
  assert.equal(await failedResearch.run(), false);
  assert.equal(failedResearch.calls.research.length, 1);
  assert.match(failedResearch.nodes["console-quickstart-status"].textContent, /首跑未完成/);
});

function loadResearchRunner({ validation, backtest, verification, spec }) {
  const nodes = {
    "research-output": { textContent: "" },
    "research-validate": { disabled: false },
    "research-run": { disabled: false },
    "research-spec": { value: spec },
  };
  const calls = [];
  let rendered = 0;
  const context = {
    state: { base: "http://127.0.0.1:18082" },
    API_PATHS: {
      appValidateDataset: "/app/validate-dataset",
      appBacktest: "/app/backtest",
      appVerify: "/app/verify",
    },
    $(id) {
      assert.ok(nodes[id], `unexpected research element: ${id}`);
      return nodes[id];
    },
    clearResearchReport() {},
    postJson: async (path, body) => {
      calls.push({ method: "json", path, body });
      return path === "/app/validate-dataset" ? validation : verification;
    },
    postJsonText: async (path, body) => {
      calls.push({ method: "text", path, body });
      return backtest;
    },
    renderResearchReport() { rendered += 1; },
  };
  const runner = sourceBetween("async function runResearch(validateOnly) {", "// 更新策略 kind");
  vm.runInNewContext(`${runner}\nglobalThis.run = runResearch;`, context);
  return { run: context.run, nodes, calls, rendered: () => rendered };
}

test("verified research flow validates, submits the original spec bytes, then verifies artifacts", async () => {
  const spec = '{"run_id":"console-run","bars_path":"sample.json","initial_cash_raw":100000000000000}';
  const research = loadResearchRunner({
    spec,
    validation: { ok: true, body: { usable: true } },
    backtest: { ok: true, body: { run_id: "console-run" } },
    verification: { ok: true, body: { verified: true } },
  });
  assert.equal(await research.run(false), true);
  assert.equal(research.calls.length, 3);
  assert.equal(research.calls[0].path, "/app/validate-dataset");
  assert.deepEqual(JSON.parse(JSON.stringify(research.calls[0].body)), {
    schema_version: 1,
    dataset_id: "console-run",
    bars_path: "sample.json",
  });
  assert.equal(research.calls[1].path, "/app/backtest");
  assert.equal(research.calls[1].body, spec, "large integer literals must remain byte-for-byte exact");
  assert.equal(research.calls[2].path, "/app/verify");
  assert.equal(research.rendered(), 1);
  assert.equal(research.nodes["research-validate"].disabled, false);
  assert.equal(research.nodes["research-run"].disabled, false);
});

test("research flow stops before the backtest when dataset validation fails", async () => {
  const research = loadResearchRunner({
    spec: '{"run_id":"console-run","bars_path":"sample.json"}',
    validation: { ok: true, body: { usable: false } },
    backtest: { ok: true, body: { run_id: "console-run" } },
    verification: { ok: true, body: { verified: true } },
  });
  assert.equal(await research.run(false), false);
  assert.deepEqual(research.calls.map((call) => call.path), ["/app/validate-dataset"]);
  assert.equal(research.rendered(), 0);
});
