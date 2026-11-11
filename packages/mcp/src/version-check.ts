/**
 * Stale-server warning.
 *
 * A long-lived MCP config can keep serving an old build for weeks (`npx`
 * caches, a pinned version, a parked checkout). At boot the server asks the
 * public npm registry — not a vcad-operated service — for the newest
 * `@vcad/mcp` on the `latest` dist-tag, compares it with its own version, and
 * reports the verdict in `server_info` and on tool results.
 *
 * Best-effort and offline-safe: a failed or slow lookup leaves the status
 * `unknown`, never an error. Opt out with `VCAD_MCP_NO_UPDATE_CHECK=1`.
 */

export const NPM_LATEST_URL = "https://registry.npmjs.org/@vcad/mcp/latest";

export type UpdateStatus =
  | { state: "unknown"; running: string; reason: string }
  | { state: "current"; running: string; latest: string }
  | { state: "stale"; running: string; latest: string; message: string };

interface ParsedVersion {
  core: [number, number, number];
  pre: Array<string | number>;
}

function parse(v: string): ParsedVersion | null {
  const m = /^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+.*)?$/.exec(v.trim());
  if (!m) return null;
  return {
    core: [Number(m[1]), Number(m[2]), Number(m[3])],
    pre: m[4] ? m[4].split(".").map((p) => (/^\d+$/.test(p) ? Number(p) : p)) : [],
  };
}

/** Semver precedence: negative if a < b, 0 if equal, positive if a > b.
 *  Returns null when either side isn't a semver string. Published builds are
 *  `<base>-main.<run>`, so the prerelease run number orders them. */
export function compareVersions(a: string, b: string): number | null {
  const pa = parse(a);
  const pb = parse(b);
  if (!pa || !pb) return null;
  for (let i = 0; i < 3; i++) {
    const d = pa.core[i]! - pb.core[i]!;
    if (d !== 0) return d;
  }
  if (pa.pre.length === 0 || pb.pre.length === 0) {
    return pb.pre.length - pa.pre.length; // release > prerelease
  }
  const n = Math.max(pa.pre.length, pb.pre.length);
  for (let i = 0; i < n; i++) {
    const x = pa.pre[i];
    const y = pb.pre[i];
    if (x === undefined) return -1;
    if (y === undefined) return 1;
    if (x === y) continue;
    if (typeof x === "number" && typeof y === "number") return x - y;
    if (typeof x === "number") return -1;
    if (typeof y === "number") return 1;
    return x < y ? -1 : 1;
  }
  return 0;
}

/** Verdict for a running version against the registry's latest. */
export function judge(running: string, latest: string): UpdateStatus {
  const cmp = compareVersions(running, latest);
  if (cmp === null) {
    return { state: "unknown", running, reason: `unparseable version (${running} vs ${latest})` };
  }
  if (cmp >= 0) return { state: "current", running, latest };
  return {
    state: "stale",
    running,
    latest,
    message:
      `This vcad MCP server is out of date: running ${running}, newest is ${latest}. ` +
      `Tools, fixes and previews may be missing. Restart the agent so ` +
      `\`npx -y @vcad/mcp\` picks up the new version (or update vcad.app).`,
  };
}

let status: UpdateStatus = { state: "unknown", running: "", reason: "not checked yet" };
let warnedInResult = false;

export function getUpdateStatus(): UpdateStatus {
  return status;
}

/** Test seam. */
export function _setUpdateStatus(s: UpdateStatus): void {
  status = s;
  warnedInResult = false;
}

/** Kick off the registry lookup. Never throws; resolves when settled. */
export async function startUpdateCheck(
  running: string,
  fetchImpl: typeof fetch = globalThis.fetch,
  timeoutMs = 3000,
): Promise<UpdateStatus> {
  if (process.env.VCAD_MCP_NO_UPDATE_CHECK === "1") {
    status = { state: "unknown", running, reason: "disabled (VCAD_MCP_NO_UPDATE_CHECK=1)" };
    return status;
  }
  status = { state: "unknown", running, reason: "checking" };
  try {
    const ctrl = new AbortController();
    const t = setTimeout(() => ctrl.abort(), timeoutMs);
    t.unref?.();
    const res = await fetchImpl(NPM_LATEST_URL, {
      signal: ctrl.signal,
      headers: { accept: "application/json" },
    });
    clearTimeout(t);
    if (!res.ok) throw new Error(`registry HTTP ${res.status}`);
    const body = (await res.json()) as { version?: unknown };
    if (typeof body.version !== "string") throw new Error("registry reply has no version");
    status = judge(running, body.version);
    if (status.state === "stale") console.error(`[mcp] ${status.message}`);
  } catch (e) {
    status = { state: "unknown", running, reason: `lookup failed: ${(e as Error).message}` };
  }
  return status;
}

/**
 * Text to surface on a tool result, or null. A stale server says so on the
 * first result after detection (so the agent tells the user once) — the
 * `_meta` stamp and `server_info` carry it on every call after that.
 */
export function takeResultWarning(): string | null {
  if (status.state !== "stale" || warnedInResult) return null;
  warnedInResult = true;
  return `⚠ ${status.message}`;
}
