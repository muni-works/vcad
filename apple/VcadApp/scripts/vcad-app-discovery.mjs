// Finds which running vcad.app an agent should talk to.
//
// Each instance writes ~/.vcad/instances/<pid>.json ({pid, port, url,
// instance_id, started, document_name, document_path}); ~/.vcad/app.json is a
// legacy "most recent" pointer (also the only file older app builds write).
//
// Selection, in order:
//   1. VCAD_APP_URL — that endpoint, verbatim.
//   2. VCAD_APP_PID — that instance; an error (never a silent fallback) if it is gone.
//   3. a document path (VCAD_APP_DOCUMENT, or one the caller passes) — the
//      instance whose open document is that file.
//   4. the most recently launched live instance, with a warning naming the others.
//
// Dev builds (bundle_id io.vcad.dev.*) are skipped in 3–4 unless nothing else
// is running, so a harness's throwaway bundle can't win "newest". Files naming
// a dead pid are deleted on read (crashed apps never clean up).
//
// Mirrored in packages/mcp/src/app-discovery.ts (keep the two in step).
import { readdirSync, readFileSync, unlinkSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

export function vcadHome(home = homedir()) {
  return join(home, ".vcad");
}

export function isAlive(pid) {
  if (typeof pid !== "number" || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return e.code === "EPERM";
  }
}

const urlOf = (r) => r.url ?? `http://127.0.0.1:${r.port}/mcp`;

export const DEV_BUNDLE_PREFIX = "io.vcad.dev.";

export const isDevBuild = (r) => typeof r.bundle_id === "string" && r.bundle_id.startsWith(DEV_BUNDLE_PREFIX);

/**
 * Live instance records, newest first. Falls back to app.json when no
 * per-instance files exist. Records naming a dead pid (and a dead app.json)
 * are deleted; unreadable files are left alone.
 */
export function listInstances({ home = homedir(), alive = isAlive } = {}) {
  const dir = join(vcadHome(home), "instances");
  const live = (p) => {
    let r;
    try {
      r = JSON.parse(readFileSync(p, "utf8"));
    } catch {
      return null;
    }
    if (typeof r?.pid !== "number" || typeof r?.port !== "number") return null;
    if (alive(r.pid)) return r;
    try {
      unlinkSync(p);
    } catch {}
    return null;
  };
  let records = [];
  try {
    records = readdirSync(dir)
      .filter((f) => f.endsWith(".json"))
      .map((f) => live(join(dir, f)))
      .filter(Boolean);
  } catch {}
  const legacy = live(join(vcadHome(home), "app.json"));
  if (records.length === 0 && legacy) records = [legacy];
  return records.sort((a, b) => String(b.started ?? "").localeCompare(String(a.started ?? "")));
}

export function describe(r) {
  const doc = r.document_name ?? r.document_path ?? "no document";
  return isDevBuild(r) ? `pid ${r.pid} (${doc}, dev build ${r.bundle_id})` : `pid ${r.pid} (${doc})`;
}

/**
 * Picks the target. Returns {url, instance, warning?} or {error}.
 * `instances` are live records, newest first (listInstances()).
 */
export function selectInstance({ env = process.env, instances, documentPath } = {}) {
  if (env.VCAD_APP_URL) {
    const url = env.VCAD_APP_URL;
    const instance = instances.find((r) => urlOf(r) === url) ?? null;
    return { url, instance };
  }
  if (env.VCAD_APP_PID) {
    const pid = Number(env.VCAD_APP_PID);
    const instance = instances.find((r) => r.pid === pid);
    if (!instance) {
      const others = instances.length ? ` Running: ${instances.map(describe).join(", ")}.` : "";
      return { error: `VCAD_APP_PID=${env.VCAD_APP_PID}: no running vcad.app with that pid.${others}` };
    }
    return { url: urlOf(instance), instance };
  }
  if (instances.length === 0) return { error: "no running vcad.app found (~/.vcad/instances is empty)" };
  const real = instances.filter((r) => !isDevBuild(r));
  const dev = instances.filter(isDevBuild);
  const onlyDev = real.length === 0;
  const pool = onlyDev ? dev : real;
  const wanted = documentPath ?? env.VCAD_APP_DOCUMENT;
  if (wanted) {
    const abs = resolve(wanted);
    const instance = pool.find((r) => r.document_path && resolve(r.document_path) === abs);
    if (instance) return { url: urlOf(instance), instance };
  }
  const [instance, ...others] = pool;
  const notes = [];
  if (others.length)
    notes.push(
      `${pool.length} vcad.app instances are running; using the newest, ${describe(instance)}. ` +
        `Others: ${others.map(describe).join(", ")}. Set VCAD_APP_PID to choose.`,
    );
  if (onlyDev) notes.push(`Only dev builds (${DEV_BUNDLE_PREFIX}*) are running; using ${describe(instance)}. Set VCAD_APP_PID to pin one.`);
  else if (dev.length) notes.push(`Skipped dev build(s) ${dev.map(describe).join(", ")}; set VCAD_APP_PID to use one.`);
  return { url: urlOf(instance), instance, ...(notes.length ? { warning: notes.join(" ") } : {}) };
}

/**
 * Tracks which instance answered last. Returns a notice when the instance id
 * changed (the app relaunched, or a different window took over), else null.
 */
export function relaunchTracker() {
  let last = null;
  return (meta) => {
    if (!meta?.instance_id) return null;
    const prev = last;
    last = meta;
    if (!prev || prev.instance_id === meta.instance_id) return null;
    return (
      `vcad.app CHANGED under this session: was ${describe({ pid: prev.pid, document_name: prev.document })}, ` +
      `now ${describe({ pid: meta.pid, document_name: meta.document })}. The app relaunched or another window ` +
      `took over; earlier state (highlights, pose, open document) is gone. Re-check with app_state.`
    );
  };
}
