/**
 * Finds which running vcad.app this server links to. Mirror of
 * apple/VcadApp/scripts/vcad-app-discovery.mjs (keep the two in step).
 *
 * Each instance writes ~/.vcad/instances/<pid>.json; ~/.vcad/app.json is a
 * legacy "most recent" pointer (the only file older app builds write).
 * Selection: VCAD_APP_URL → VCAD_APP_PID (error if gone, never a silent
 * fallback) → the instance whose open document matches → the newest, with a
 * warning naming the others.
 */
import { readdirSync, readFileSync } from "node:fs";
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
};

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

export function describeInstance(r: Pick<InstanceRecord, "pid" | "document_name" | "document_path">): string {
  return `pid ${r.pid} (${r.document_name ?? r.document_path ?? "no document"})`;
}

/** Live instance records, newest first. */
export function listInstances(
  { home = homedir(), alive = isAlive }: { home?: string; alive?: (pid: unknown) => boolean } = {},
): InstanceRecord[] {
  const root = join(home, ".vcad");
  const read = (p: string): InstanceRecord | null => {
    try {
      return JSON.parse(readFileSync(p, "utf8")) as InstanceRecord;
    } catch {
      return null;
    }
  };
  let records: InstanceRecord[] = [];
  try {
    const dir = join(root, "instances");
    records = readdirSync(dir)
      .filter((f) => f.endsWith(".json"))
      .map((f) => read(join(dir, f)))
      .filter((r): r is InstanceRecord => r !== null);
  } catch {
    // no instances dir: an older app, or none running
  }
  if (records.length === 0) {
    const legacy = read(join(root, "app.json"));
    if (legacy) records = [legacy];
  }
  return records
    .filter((r) => alive(r.pid))
    .sort((a, b) => String(b.started ?? "").localeCompare(String(a.started ?? "")));
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
  const wanted = documentPath ?? env.VCAD_APP_DOCUMENT;
  if (wanted) {
    const abs = resolve(wanted);
    const instance = instances.find((r) => r.document_path && resolve(r.document_path) === abs);
    if (instance) return { url: urlOf(instance), instance };
  }
  const [instance, ...others] = instances;
  if (!others.length) return { url: urlOf(instance), instance };
  return {
    url: urlOf(instance),
    instance,
    warning:
      `${instances.length} vcad.app instances are running; using the newest, ${describeInstance(instance)}. ` +
      `Others: ${others.map(describeInstance).join(", ")}. Set VCAD_APP_PID to choose.`,
  };
}
