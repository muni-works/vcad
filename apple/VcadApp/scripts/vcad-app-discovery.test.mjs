// node --test apple/VcadApp/scripts/vcad-app-discovery.test.mjs
import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { listInstances, relaunchTracker, selectInstance } from "./vcad-app-discovery.mjs";

const rec = (pid, started, document_path) => ({
  pid, port: 47000 + pid, url: `http://127.0.0.1:${47000 + pid}/mcp`, instance_id: `id-${pid}`, started,
  document_name: document_path?.split("/").pop() ?? null, document_path: document_path ?? null,
});

test("lists live instances newest first, legacy fallback", () => {
  const h = mkdtempSync(join(tmpdir(), "vcad-disc-"));
  mkdirSync(join(h, ".vcad", "instances"), { recursive: true });
  writeFileSync(join(h, ".vcad", "app.json"), JSON.stringify(rec(9, "2026-11-11T09:00:00Z")));
  assert.deepEqual(listInstances({ home: h, alive: () => true }).map((r) => r.pid), [9]);
  for (const r of [rec(1, "2026-11-11T10:00:00Z"), rec(2, "2026-11-11T11:00:00Z")])
    writeFileSync(join(h, ".vcad", "instances", `${r.pid}.json`), JSON.stringify(r));
  assert.deepEqual(listInstances({ home: h, alive: () => true }).map((r) => r.pid), [2, 1]);
  assert.deepEqual(listInstances({ home: h, alive: (p) => p === 1 }).map((r) => r.pid), [1]);
});

test("selection order", () => {
  const a = rec(1, "2026-11-11T10:00:00Z", "/tmp/a.vcad");
  const b = rec(2, "2026-11-11T11:00:00Z", "/tmp/b.vcad");
  const instances = [b, a];
  assert.equal(selectInstance({ env: { VCAD_APP_URL: "http://x/mcp" }, instances }).url, "http://x/mcp");
  assert.equal(selectInstance({ env: { VCAD_APP_PID: "1" }, instances }).instance.pid, 1);
  assert.match(selectInstance({ env: { VCAD_APP_PID: "5" }, instances }).error, /VCAD_APP_PID=5/);
  assert.equal(selectInstance({ env: { VCAD_APP_DOCUMENT: "/tmp/a.vcad" }, instances }).instance.pid, 1);
  const newest = selectInstance({ env: {}, instances });
  assert.equal(newest.instance.pid, 2);
  assert.match(newest.warning, /Others: pid 1/);
});

test("prunes dead-pid files, keeps live and unreadable ones", () => {
  const h = mkdtempSync(join(tmpdir(), "vcad-disc-"));
  const dir = join(h, ".vcad", "instances");
  mkdirSync(dir, { recursive: true });
  for (const r of [rec(1, "2026-11-11T10:00:00Z"), rec(3, "2026-11-11T12:00:00Z")])
    writeFileSync(join(dir, `${r.pid}.json`), JSON.stringify(r));
  writeFileSync(join(h, ".vcad", "app.json"), JSON.stringify(rec(3, "2026-11-11T12:00:00Z")));
  writeFileSync(join(dir, "junk.json"), "not a record");
  assert.deepEqual(listInstances({ home: h, alive: (p) => p === 1 }).map((r) => r.pid), [1]);
  assert.equal(existsSync(join(dir, "3.json")), false);
  assert.equal(existsSync(join(h, ".vcad", "app.json")), false);
  assert.equal(existsSync(join(dir, "1.json")), true);
  assert.equal(existsSync(join(dir, "junk.json")), true);
});

test("dev builds only when named or alone", () => {
  const real = { ...rec(1, "2026-11-16T09:00:00Z", "/tmp/a.vcad"), bundle_id: "io.vcad.m0" };
  const shots = { ...rec(3, "2026-11-16T10:00:00Z", "/tmp/a.vcad"), bundle_id: "io.vcad.dev.app-sim-screens-fbe447" };
  const s = selectInstance({ env: {}, instances: [shots, real] });
  assert.equal(s.instance.pid, 1);
  assert.match(s.warning, /Skipped dev build\(s\) pid 3/);
  assert.equal(selectInstance({ env: {}, instances: [shots, real], documentPath: "/tmp/a.vcad" }).instance.pid, 1);
  assert.equal(selectInstance({ env: {}, instances: [shots, rec(2, "2026-11-16T08:00:00Z")] }).instance.pid, 2);
  assert.equal(selectInstance({ env: { VCAD_APP_PID: "3" }, instances: [shots, real] }).instance.pid, 3);
  const alone = selectInstance({ env: {}, instances: [shots] });
  assert.equal(alone.instance.pid, 3);
  assert.match(alone.warning, /Only dev builds/);
});

test("relaunch tracker flags an instance id change once", () => {
  const seen = relaunchTracker();
  assert.equal(seen({ pid: 1, instance_id: "a", document: "x.vcad" }), null);
  assert.equal(seen({ pid: 1, instance_id: "a", document: "x.vcad" }), null);
  assert.match(seen({ pid: 2, instance_id: "b", document: null }), /CHANGED.*was pid 1 \(x.vcad\).*now pid 2/);
  assert.equal(seen({ pid: 2, instance_id: "b" }), null);
  assert.equal(seen(undefined), null);
});
