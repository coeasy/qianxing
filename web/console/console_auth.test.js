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
  await sample.install();
  assert.deepEqual(sample.calls, [{ path: "/console/sample-data", body: "" }]);
  assert.match(sample.output.textContent, /教学数据已安装/);
  assert.equal(sample.button.disabled, false);
});

test("static Console never sends the BFF-only sample installation write", async () => {
  const sample = loadSampleInstaller({ connected: true, viaBff: false, response: null });
  await sample.install();
  assert.equal(sample.calls.length, 0);
  assert.match(sample.output.textContent, /仅由同源 Console 提供/);
});
