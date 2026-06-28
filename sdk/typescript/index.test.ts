/**
 * lvsandbox TypeScript SDK — integration tests.
 *
 * Requires a running sandbox-server at LVSANDBOX_URL (default http://127.0.0.1:8080).
 * Skipped if server not reachable.
 *
 * Run: node --import tsx --test index.test.ts
 */

import { describe, it } from "node:test";
import * as assert from "node:assert/strict";
import { LvSandbox } from "./index.js";

const BASE_URL = process.env["LVSANDBOX_URL"] ?? "http://127.0.0.1:8080";

async function isServerUp(): Promise<boolean> {
  const s = new LvSandbox({ baseUrl: BASE_URL });
  try {
    await s.health();
    return true;
  } catch {
    return false;
  }
}

const skip = !(await isServerUp());

// ---------------------------------------------------------------------------
// Jobs
// ---------------------------------------------------------------------------

describe("jobs", () => {
  const sandbox = new LvSandbox({ baseUrl: BASE_URL });

  it("run echo returns stdout", { skip }, async () => {
    const r = await sandbox.jobs.run(["/bin/echo", "hello-ts"]);
    assert.equal(r.status, "Completed");
    assert.ok(r.stdout.includes("hello-ts"), `stdout: ${r.stdout}`);
    assert.equal(r.exit_code, 0);
  });

  it("run with timeout results in TimedOut", { skip }, async () => {
    const r = await sandbox.jobs.run(["/bin/sleep", "30"], { timeout: "1s" });
    assert.equal(r.status, "TimedOut");
    assert.equal(r.timed_out, true);
  });

  // profile 检测在异步任务内; jobs.run 会在 poll 时报错
  // 该路径由 Rust 侧 tests 覆盖,TS SDK 不重复测
});

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

describe("sessions", () => {
  const sandbox = new LvSandbox({ baseUrl: BASE_URL });

  it("create, exec, files, delete", { skip }, async () => {
    const sid = await sandbox.sessions.create("shell");
    assert.ok(sid.length > 0);

    const sessions = await sandbox.sessions.list();
    assert.ok(sessions.some((s) => s.session_id === sid));

    const info = await sandbox.sessions.get(sid);
    assert.ok(info);
    assert.equal(info!.session_id, sid);

    // exec write file
    const r = await sandbox.sessions.exec(sid, ["/bin/sh", "-c", "echo ts-e2e > ts-out.txt"]);
    assert.equal(r.status, "Completed");

    const data = await sandbox.sessions.files.get(sid, "ts-out.txt");
    const text = new TextDecoder().decode(data);
    assert.ok(text.includes("ts-e2e"), `file: ${text}`);

    // put file
    await sandbox.sessions.files.put(sid, "uploaded.ts", "console.log(42);");
    const entries = await sandbox.sessions.files.list(sid);
    assert.ok(entries.some((e) => e.name === "uploaded.ts"));

    await sandbox.sessions.delete(sid);
    assert.equal(await sandbox.sessions.get(sid), null);
  });
});

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

describe("snapshots", () => {
  const sandbox = new LvSandbox({ baseUrl: BASE_URL });

  it("create, list, delete", { skip }, async () => {
    const sid = await sandbox.sessions.create("shell");
    await sandbox.sessions.exec(sid, ["/bin/sh", "-c", "echo snap > s.txt"]);
    const snapId = await sandbox.sessions.snapshot(sid);
    assert.ok(snapId.length > 0);
    const snaps = await sandbox.snapshots.list();
    assert.ok(snaps.includes(snapId));
    await sandbox.snapshots.delete(snapId);
    await sandbox.sessions.delete(sid);
  });
});

// ---------------------------------------------------------------------------
// Volumes
// ---------------------------------------------------------------------------

describe("volumes", () => {
  const sandbox = new LvSandbox({ baseUrl: BASE_URL });

  it("create, list, delete", { skip }, async () => {
    await sandbox.volumes.create("ts-vol");
    const vols = await sandbox.volumes.list();
    assert.ok(vols.includes("ts-vol"));
    await sandbox.volumes.delete("ts-vol");
  });
});

// ---------------------------------------------------------------------------
// runPython
// ---------------------------------------------------------------------------

describe("runPython", () => {
  const sandbox = new LvSandbox({ baseUrl: BASE_URL });

  it("executes python code", { skip }, async () => {
    const { result, files } = await sandbox.runPython("print(42)");
    assert.equal(result.status, "Completed");
    assert.ok(result.stdout.includes("42"), `stdout: ${result.stdout}`);
  });
});

// ---------------------------------------------------------------------------
// openaiToolSchema
// ---------------------------------------------------------------------------

describe("openaiToolSchema", () => {
  it("returns valid OpenAI tool schema", () => {
    const sandbox = new LvSandbox();
    const schema = sandbox.openaiToolSchema();
    assert.equal(schema.type, "function");
    assert.equal(schema.function.name, "run_python");
    assert.ok(schema.function.parameters.required.includes("code"));
  });
});

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

describe("error handling", () => {
  it("connection refused throws", async () => {
    const bad = new LvSandbox({ baseUrl: "http://127.0.0.1:19999" });
    await assert.rejects(() => bad.health());
  });
});
