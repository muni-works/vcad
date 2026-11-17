import { existsSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { listInstances, selectInstance, type InstanceRecord } from "../app-discovery.js";

const rec = (pid: number, started: string, document_path?: string): InstanceRecord => ({
  pid,
  port: 47000 + pid,
  url: `http://127.0.0.1:${47000 + pid}/mcp`,
  instance_id: `id-${pid}`,
  started,
  document_name: document_path?.split("/").pop() ?? null,
  document_path: document_path ?? null,
});

function home(records: InstanceRecord[], legacy?: InstanceRecord): string {
  const h = mkdtempSync(join(tmpdir(), "vcad-disc-"));
  mkdirSync(join(h, ".vcad", "instances"), { recursive: true });
  for (const r of records) writeFileSync(join(h, ".vcad", "instances", `${r.pid}.json`), JSON.stringify(r));
  if (legacy) writeFileSync(join(h, ".vcad", "app.json"), JSON.stringify(legacy));
  return h;
}

describe("listInstances", () => {
  it("returns live instances newest first and drops dead pids", () => {
    const h = home([rec(1, "2026-11-11T10:00:00.000Z"), rec(2, "2026-11-11T11:00:00.000Z"), rec(3, "2026-11-11T12:00:00.000Z")]);
    const live = listInstances({ home: h, alive: (pid) => pid !== 3 });
    expect(live.map((r) => r.pid)).toEqual([2, 1]);
  });
  it("falls back to the legacy app.json when no per-instance files exist", () => {
    const h = home([], rec(9, "2026-11-11T10:00:00.000Z"));
    expect(listInstances({ home: h, alive: () => true }).map((r) => r.pid)).toEqual([9]);
  });
  it("ignores a stale legacy pointer once per-instance files exist", () => {
    const h = home([rec(1, "2026-11-11T10:00:00.000Z")], rec(9, "2026-11-11T12:00:00.000Z"));
    expect(listInstances({ home: h, alive: () => true }).map((r) => r.pid)).toEqual([1]);
  });
  it("deletes files naming a dead pid, keeps live and unreadable ones", () => {
    const h = home([rec(1, "2026-11-11T10:00:00.000Z"), rec(3, "2026-11-11T12:00:00.000Z")], rec(3, "2026-11-11T12:00:00.000Z"));
    const dir = join(h, ".vcad", "instances");
    writeFileSync(join(dir, "junk.json"), "not a record");
    writeFileSync(join(dir, "nopid.json"), JSON.stringify({ port: 1 }));
    expect(listInstances({ home: h, alive: (pid) => pid === 1 }).map((r) => r.pid)).toEqual([1]);
    expect(existsSync(join(dir, "3.json"))).toBe(false);
    expect(existsSync(join(h, ".vcad", "app.json"))).toBe(false);
    expect(existsSync(join(dir, "1.json"))).toBe(true);
    expect(existsSync(join(dir, "junk.json"))).toBe(true);
    expect(existsSync(join(dir, "nopid.json"))).toBe(true);
  });
  it("keeps a live app.json pointer", () => {
    const h = home([rec(1, "2026-11-11T10:00:00.000Z")], rec(1, "2026-11-11T10:00:00.000Z"));
    listInstances({ home: h, alive: () => true });
    expect(existsSync(join(h, ".vcad", "app.json"))).toBe(true);
  });
});

describe("selectInstance", () => {
  const a = rec(1, "2026-11-11T10:00:00.000Z", "/tmp/a.vcad");
  const b = rec(2, "2026-11-11T11:00:00.000Z", "/tmp/b.vcad");
  const instances = [b, a];

  it("VCAD_APP_URL wins verbatim", () => {
    const s = selectInstance({ env: { VCAD_APP_URL: a.url }, instances });
    expect(s.url).toBe(a.url);
    expect(s.instance?.pid).toBe(1);
  });
  it("VCAD_APP_PID picks that instance", () => {
    expect(selectInstance({ env: { VCAD_APP_PID: "1" }, instances }).instance?.pid).toBe(1);
  });
  it("VCAD_APP_PID for a dead pid is an error, not a fallback", () => {
    const s = selectInstance({ env: { VCAD_APP_PID: "77" }, instances });
    expect(s.url).toBeUndefined();
    expect(s.error).toMatch(/77.*Running: pid 2/);
  });
  it("matches the open document path", () => {
    expect(selectInstance({ env: {}, instances, documentPath: "/tmp/a.vcad" }).instance?.pid).toBe(1);
    expect(selectInstance({ env: { VCAD_APP_DOCUMENT: "/tmp/../tmp/a.vcad" }, instances }).instance?.pid).toBe(1);
  });
  it("defaults to the newest with a warning listing the others", () => {
    const s = selectInstance({ env: {}, instances });
    expect(s.instance?.pid).toBe(2);
    expect(s.warning).toMatch(/2 vcad.app instances.*pid 2.*Others: pid 1 \(a.vcad\).*VCAD_APP_PID/);
  });
  it("no warning with a single instance; error with none", () => {
    expect(selectInstance({ env: {}, instances: [a] }).warning).toBeUndefined();
    expect(selectInstance({ env: {}, instances: [] }).error).toMatch(/no running vcad.app/);
  });
});

describe("dev builds (io.vcad.dev.*)", () => {
  const real = { ...rec(1, "2026-11-16T09:00:00.000Z", "/tmp/a.vcad"), bundle_id: "io.vcad.m0" };
  const old = rec(2, "2026-11-16T08:00:00.000Z"); // pre-bundle_id build: not dev
  const shots = { ...rec(3, "2026-11-16T10:00:00.000Z", "/tmp/a.vcad"), bundle_id: "io.vcad.dev.app-sim-screens-fbe447" };

  it("a newer dev build loses to the real app, with a warning naming it", () => {
    const s = selectInstance({ env: {}, instances: [shots, real] });
    expect(s.instance?.pid).toBe(1);
    expect(s.warning).toMatch(/Skipped dev build\(s\) pid 3 \(a.vcad, dev build io.vcad.dev.app-sim-screens-fbe447\)/);
  });
  it("even on a document match", () => {
    expect(selectInstance({ env: {}, instances: [shots, real], documentPath: "/tmp/a.vcad" }).instance?.pid).toBe(1);
  });
  it("a record with no bundle id still beats a dev build", () => {
    expect(selectInstance({ env: {}, instances: [shots, old] }).instance?.pid).toBe(2);
  });
  it("is used when named by VCAD_APP_PID or VCAD_APP_URL", () => {
    expect(selectInstance({ env: { VCAD_APP_PID: "3" }, instances: [shots, real] }).instance?.pid).toBe(3);
    expect(selectInstance({ env: { VCAD_APP_URL: shots.url }, instances: [shots, real] }).instance?.pid).toBe(3);
  });
  it("is used, with a warning, when nothing else is running", () => {
    const s = selectInstance({ env: {}, instances: [shots] });
    expect(s.instance?.pid).toBe(3);
    expect(s.warning).toMatch(/Only dev builds/);
  });
});
