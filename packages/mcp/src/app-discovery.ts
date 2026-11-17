/**
 * Finds which running vcad.app this server links to. Mirror of
 * apple/VcadApp/scripts/vcad-app-discovery.mjs (keep the two in step).
 *
 * Each instance writes ~/.vcad/instances/<pid>.json; ~/.vcad/app.json is a
 * legacy "most recent" pointer (the only file older app builds write).
 * Selection: VCAD_APP_URL → VCAD_APP_PID (error if gone, never a silent
 * fallback) → the instance whose open document matches → the newest, with a
 * warning naming the others.
 *
 * Dev builds (bundle_id io.vcad.dev.*, every non-release bundle.sh build) are
 * picked only when VCAD_APP_URL / VCAD_APP_PID names them or nothing else is
 * running, so a harness's throwaway bundle can't win "newest" and strand a
 * real session. Files naming a dead pid are deleted on read (crashed or
 * SIGKILLed apps never clean up), like crates/vcad-mcp/src/discovery.rs.
 */
import { readdirSync, readFileSync, unlinkSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";

export type InstanceRecord = {
  pid: number;
  port: number;
  url?: string;
  instance_id?: string;
  started?: string;
  document_name?: string | null;
  document_path?: string | null;
  /** CFBundleIdentifier; absent in records from older builds. */
  bundle_id?: string | null;
};

/** Bundle-id prefix of bundle.sh dev builds. */
export const DEV_BUNDLE_PREFIX = "io.vcad.dev.";

export const isDevBuild = (r: Pick<InstanceRecord, "bundle_id">): boolean =>
  typeof r.bundle_id === "string" && r.bundle_id.startsWith(DEV_BUNDLE_PREFIX);

export type Selection =
  | { url: string; instance: InstanceRecord | null; warning?: string; error?: undefined }
  | { error: string; url?: undefined; instance?: undefined; warning?: undefined };

export function isAlive(pid: unknown): boolean {
  if (typeof pid !== "number" || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return (e as NodeJS.ErrnoException).code === "EPERM";
  }
}

const urlOf = (r: InstanceRecord) => r.url ?? `http://127.0.0.1:${r.port}/mcp`;

export function describeInstance(
  r: Pick<InstanceRecord, "pid" | "document_name" | "document_path"> & Pick<Partial<InstanceRecord>, "bundle_id">,
): string {
  const doc = r.document_name ?? r.document_path ?? "no document";
  return isDevBuild(r) ? `pid ${r.pid} (${doc}, dev build ${r.bundle_id})` : `pid ${r.pid} (${doc})`;
}

/**
 * Live instance records, newest first. Files that parse as a record but name a
 * dead pid are deleted (unreadable files are left alone), and so is a dead
 * app.json pointer; the pointer is only read when no per-instance file exists.
 */
export function listInstances(
  { home = homedir(), alive = isAlive }: { home?: string; alive?: (pid: unknown) => boolean } = {},
): InstanceRecord[] {
  const root = join(home, ".vcad");
  const read = (p: string): InstanceRecord | null => {
    try {
      const r = JSON.parse(readFileSync(p, "utf8")) as InstanceRecord;
      return typeof r?.pid === "number" && typeof r?.port === "number" ? r : null;
    } catch {
      return null;
    }
  };
  /** The record at `p` if its pid is alive; deletes the file if it is dead. */
  const live = (p: string): InstanceRecord | null => {
    const r = read(p);
    if (!r) return null;
    if (alive(r.pid)) return r;
    try {
      unlinkSync(p);
    } catch {
      // already gone, or not ours to delete
    }
    return null;
  };
  let records: InstanceRecord[] = [];
  try {
    const dir = join(root, "instances");
    records = readdirSync(dir)
      .filter((f) => f.endsWith(".json"))
      .map((f) => live(join(dir, f)))
      .filter((r): r is InstanceRecord => r !== null);
  } catch {
    // no instances dir: an older app, or none running
  }
  const legacy = live(join(root, "app.json"));
  if (records.length === 0 && legacy) records = [legacy];
  return records.sort((a, b) => String(b.started ?? "").localeCompare(String(a.started ?? "")));
}

export function selectInstance({
  env = process.env,
  instances,
  documentPath,
}: {
  env?: NodeJS.ProcessEnv;
  instances: InstanceRecord[];
  documentPath?: string;
}): Selection {
  if (env.VCAD_APP_URL) {
    const url = env.VCAD_APP_URL;
    return { url, instance: instances.find((r) => urlOf(r) === url) ?? null };
  }
  if (env.VCAD_APP_PID) {
    const pid = Number(env.VCAD_APP_PID);
    const instance = instances.find((r) => r.pid === pid);
    if (!instance) {
      const others = instances.length ? ` Running: ${instances.map(describeInstance).join(", ")}.` : "";
      return { error: `VCAD_APP_PID=${env.VCAD_APP_PID}: no running vcad.app with that pid.${others}` };
    }
    return { url: urlOf(instance), instance };
  }
  if (instances.length === 0) return { error: "no running vcad.app found (~/.vcad/instances is empty)" };
  // Dev builds only by name (above) or when nothing else is running.
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
  const notes: string[] = [];
  if (others.length)
    notes.push(
      `${pool.length} vcad.app instances are running; using the newest, ${describeInstance(instance)}. ` +
        `Others: ${others.map(describeInstance).join(", ")}. Set VCAD_APP_PID to choose.`,
    );
  if (onlyDev)
    notes.push(`Only dev builds (${DEV_BUNDLE_PREFIX}*) are running; using ${describeInstance(instance)}. Set VCAD_APP_PID to pin one.`);
  else if (dev.length) notes.push(`Skipped dev build(s) ${dev.map(describeInstance).join(", ")}; set VCAD_APP_PID to use one.`);
  return notes.length ? { url: urlOf(instance), instance, warning: notes.join(" ") } : { url: urlOf(instance), instance };
}
