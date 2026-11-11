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
// Mirrored in packages/mcp/src/app-discovery.ts (keep the two in step).
import { readdirSync, readFileSync } from "node:fs";
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

/** Live instance records, newest first. Falls back to app.json when no per-instance files exist. */
export function listInstances({ home = homedir(), alive = isAlive } = {}) {
  const dir = join(vcadHome(home), "instances");
  let records = [];
  try {
    records = readdirSync(dir)
      .filter((f) => f.endsWith(".json"))
      .map((f) => {
        try {
          return JSON.parse(readFileSync(join(dir, f), "utf8"));
        } catch {
          return null;
        }
      })
      .filter(Boolean);
  } catch {}
  if (records.length === 0) {
    try {
      records = [JSON.parse(readFileSync(join(vcadHome(home), "app.json"), "utf8"))];
    } catch {}
  }
  return records
    .filter((r) => alive(r.pid))
    .sort((a, b) => String(b.started ?? "").localeCompare(String(a.started ?? "")));
}

export function describe(r) {
  return `pid ${r.pid} (${r.document_name ?? r.document_path ?? "no document"})`;
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
  const wanted = documentPath ?? env.VCAD_APP_DOCUMENT;
  if (wanted) {
    const abs = resolve(wanted);
    const instance = instances.find((r) => r.document_path && resolve(r.document_path) === abs);
    if (instance) return { url: urlOf(instance), instance };
  }
  const [instance, ...others] = instances;
  const warning = others.length
    ? `${instances.length} vcad.app instances are running; using the newest, ${describe(instance)}. ` +
      `Others: ${others.map(describe).join(", ")}. Set VCAD_APP_PID to choose.`
    : undefined;
  return { url: urlOf(instance), instance, ...(warning ? { warning } : {}) };
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
