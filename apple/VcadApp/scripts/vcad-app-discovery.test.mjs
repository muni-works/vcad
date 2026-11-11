// node --test apple/VcadApp/scripts/vcad-app-discovery.test.mjs
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
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

test("relaunch tracker flags an instance id change once", () => {
  const seen = relaunchTracker();
  assert.equal(seen({ pid: 1, instance_id: "a", document: "x.vcad" }), null);
  assert.equal(seen({ pid: 1, instance_id: "a", document: "x.vcad" }), null);
  assert.match(seen({ pid: 2, instance_id: "b", document: null }), /CHANGED.*was pid 1 \(x.vcad\).*now pid 2/);
  assert.equal(seen({ pid: 2, instance_id: "b" }), null);
  assert.equal(seen(undefined), null);
});
