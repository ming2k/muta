#!/usr/bin/env node
/**
 * End-to-end smoke against a live daemon (ADR-0105): the exact flow the web
 * panel drives, over the exact transports it uses.
 *
 *   1. GET /healthz                → version/auth/panel flags
 *   2. GET /                       → the embedded panel HTML
 *   3. WS monitor with token as `bearer.` subprotocol → snapshot
 *   4. Control create_session      → ControlReply with session_id
 *   5. Attach that session         → Welcome
 *   6. WS without credentials      → must fail (when auth is on)
 *   7. Version-skewed Select       → Error frame with code version_mismatch
 *
 * Env: DAEMON_URL (default http://127.0.0.1:9800), DAEMON_TOKEN (required
 * when the daemon has local_auth on), CLIENT_VERSION (defaults to the web
 * package version, matching the production client's single source of truth).
 * Exits non-zero on the first failed step.
 */

import { readFileSync } from "node:fs";

const BASE = (process.env.DAEMON_URL ?? "http://127.0.0.1:9800").replace(/\/+$/, "");
const WS_URL = BASE.replace(/^http:/, "ws:").replace(/^https:/, "wss:");
const TOKEN = process.env.DAEMON_TOKEN ?? "";
const packageJson = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));
const VERSION = process.env.CLIENT_VERSION ?? packageJson.version;

let step = "startup";
const timer = setTimeout(() => fail(step, "timeout"), 30_000);

function fail(where, why) {
  console.error(`FAIL [${where}]: ${why}`);
  process.exit(1);
}

function ok(msg) {
  console.log(`ok: ${msg}`);
}

function wsConnect(action, { token = TOKEN, version = VERSION } = {}) {
  return new Promise((resolvePromise, reject) => {
    const ws = new WebSocket(WS_URL, token ? [`bearer.${token}`] : []);
    const rejectOnce = () => reject(new Error("handshake refused"));
    ws.onerror = rejectOnce;
    ws.onclose = (e) => {
      if (!e.wasClean) rejectOnce();
    };
    ws.onopen = () => {
      ws.send(JSON.stringify({ type: "Select", action, version }));
      resolvePromise(ws);
    };
  });
}

function nextFrame(ws, predicate, label) {
  return new Promise((resolvePromise, reject) => {
    const onMessage = (e) => {
      const frame = JSON.parse(e.data);
      if (predicate(frame)) {
        ws.removeEventListener("message", onMessage);
        resolvePromise(frame);
      } else if (frame.type === "Error") {
        reject(new Error(`${label}: error frame: ${frame.message}`));
      }
    };
    ws.addEventListener("message", onMessage);
  });
}

async function main() {
  step = "healthz";
  const health = await fetch(`${BASE}/healthz`).then((r) => r.json());
  if (typeof health.version !== "string" || typeof health.auth !== "boolean") {
    fail(step, `unexpected payload ${JSON.stringify(health)}`);
  }
  if (health.auth && !TOKEN) fail(step, "daemon requires a token but DAEMON_TOKEN is unset");
  ok(`healthz ${JSON.stringify(health)}`);

  step = "static";
  const html = await fetch(`${BASE}/`).then((r) => r.text());
  if (!html.includes("<html")) fail(step, "no HTML at /");
  ok(`GET / serves the panel (${html.length} bytes)`);

  step = "monitor";
  const monitor = await wsConnect({ monitor: { watch: true, include_idle: true } }).catch((e) =>
    fail(step, e.message),
  );
  const snapshot = await nextFrame(
    monitor,
    (f) => f.type === "Monitor" && f.kind === "snapshot",
    "snapshot",
  ).catch((e) => fail(step, e.message));
  ok(`monitor snapshot (sessions=${snapshot.sessions.length})`);

  step = "create_session";
  const control = await wsConnect({ control: { verb: "create_session", project: "/" } }).catch(
    (e) => fail(step, e.message),
  );
  const reply = await nextFrame(control, (f) => f.type === "ControlReply", "control").catch((e) =>
    fail(step, e.message),
  );
  if (!reply.ok || !reply.session_id) fail(step, reply.error ?? "no session_id");
  ok(`create_session → ${reply.session_id}`);

  step = "attach";
  const session = await wsConnect({ attach: reply.session_id }).catch((e) => fail(step, e.message));
  const welcome = await nextFrame(session, (f) => f.type === "Welcome", "welcome").catch((e) =>
    fail(step, e.message),
  );
  ok(`Welcome (messages=${welcome.messages.length})`);

  if (health.auth) {
    step = "auth-negative";
    let refused = false;
    try {
      await wsConnect({ monitor: { watch: false, include_idle: true } }, { token: "" });
    } catch {
      refused = true;
    }
    if (!refused) fail(step, "credential-less handshake was accepted");
    ok("credential-less handshake refused");
  }

  step = "version-code";
  const skewed = await wsConnect(
    { monitor: { watch: false, include_idle: true } },
    { version: "0.0.0-skew" },
  ).catch((e) => fail(step, e.message));
  const err = await nextFrame(skewed, (f) => f.type === "Error", "skew").catch((e) =>
    fail(step, e.message),
  );
  if (err.code !== "version_mismatch") fail(step, `expected code version_mismatch, got ${err.code}`);
  ok("version skew refused with code=version_mismatch");

  clearTimeout(timer);
  console.log("E2E PASS");
  process.exit(0);
}

main().catch((e) => fail(step, e.message ?? String(e)));
