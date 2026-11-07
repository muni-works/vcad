/**
 * vcad.io web retirement — config and the old-route → new-behavior planner.
 *
 * vcad 2 ships as a native Mac app; the web editor, cloud sync, accounts,
 * collaboration and billing retire. See docs/launch/web-retirement.md for the
 * route table this file implements and docs/launch/vcad-url-scheme.md for the
 * `vcad://` links it hands documents off with.
 *
 * Nothing here changes behavior until `VITE_VCAD_RETIREMENT_REDIRECTS=1` is set
 * at build time — except the `/export` page, which is live on its own so users
 * can take their documents out before the switch.
 */

import { gunzipSync, gzipSync, strFromU8, strToU8 } from "fflate";
import { fromVCode } from "@vcad/ir";

/** Shutdown date shown in banners. `null` renders a "date TBA" placeholder. */
export const SHUTDOWN_DATE: string | null = null;

/** Where the native app is downloaded from. */
export const DOWNLOAD_URL = "https://vcad.io/download";

/** The export-all page. */
export const EXPORT_PATH = "/export";

/** Human shutdown copy: the date, or the placeholder. */
export function shutdownLabel(date: string | null = SHUTDOWN_DATE): string {
  return date ?? "a date to be announced";
}

/** Redirect switch. Default OFF — set `VITE_VCAD_RETIREMENT_REDIRECTS=1` to flip. */
export function retirementRedirectsEnabled(
  env: Record<string, unknown> = import.meta.env,
): boolean {
  return /^(1|true|on)$/i.test(String(env.VITE_VCAD_RETIREMENT_REDIRECTS ?? ""));
}

/** What an old vcad.io URL does after retirement. */
export type RetirementRoute =
  /** Keep serving as-is (legal pages, the export page itself). */
  | { kind: "keep" }
  /** Show the export-all page. */
  | { kind: "export" }
  /** A document carried in the URL itself → `vcad://open?doc=…`. */
  | { kind: "handoff"; appUrl: string; name?: string }
  /** A public share (`/view/<token>`, `/@user/slug`) → fetch, then hand off. */
  | { kind: "share"; token?: string; username?: string; slug?: string }
  /** A retired flow with nothing to hand off: explain and link the download. */
  | { kind: "notice"; reason: "editor" | "cli-auth" | "authorize" | "profile" | "local-doc" | "bad-link" };

function base64urlDecode(s: string): Uint8Array {
  let b64 = s.replace(/-/g, "+").replace(/_/g, "/");
  while (b64.length % 4) b64 += "=";
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function base64urlEncode(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]!);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/**
 * `vcad://open?doc=…` for a `.vcad` JSON payload. The native app loads JSON,
 * not VCode, so VCode is converted first.
 */
export function appLinkForDocumentText(text: string, name?: string | null): string {
  const trimmed = text.trim();
  const json = trimmed.startsWith("{") ? trimmed : JSON.stringify(fromVCode(trimmed));
  const params = new URLSearchParams();
  params.set("doc", base64urlEncode(gzipSync(strToU8(json), { level: 9 })));
  if (name) params.set("name", name);
  return `vcad://open?${params.toString()}`;
}

/** Inline-document URLs: `#/new?doc=<gzip+b64url>` and `?ir=<raw VCode>`. */
function inlineHandoff(loc: { search: string; hash: string }): RetirementRoute | null {
  try {
    if (loc.hash.startsWith("#/new?")) {
      const q = new URLSearchParams(loc.hash.slice(6));
      const doc = q.get("doc");
      if (!doc) return null;
      const text = q.get("raw") === "1" ? doc : strFromU8(gunzipSync(base64urlDecode(doc)));
      const name = q.get("name") ?? undefined;
      return { kind: "handoff", appUrl: appLinkForDocumentText(text, name), name };
    }
    const ir = new URLSearchParams(loc.search).get("ir");
    if (ir) {
      const name = new URLSearchParams(loc.search).get("name") ?? undefined;
      return { kind: "handoff", appUrl: appLinkForDocumentText(ir, name), name };
    }
  } catch {
    return { kind: "notice", reason: "bad-link" };
  }
  return null;
}

/**
 * Paths retirement must never touch: `/desktop` and everything under it
 * (Sparkle appcast, desktop download page). Exported so the guard is testable.
 */
export function isProtectedPath(pathname: string): boolean {
  return /^\/desktop(\/|$)/i.test(pathname);
}

/** Map an old vcad.io location to its post-retirement behavior. */
export function planRetirementRoute(loc: {
  pathname: string;
  search: string;
  hash: string;
}): RetirementRoute {
  const path = loc.pathname.replace(/\/+$/, "") || "/";

  // RELEASE-BLOCKING: the Sparkle update feed (SUFeedURL =
  // https://vcad.io/desktop/appcast.xml, apple/VcadApp/bundle.sh) and the
  // /desktop/ download page must keep serving unchanged forever — every
  // installed copy of the Mac app checks it for updates. Checked first so no
  // later rule (or flag) can ever catch it.
  if (isProtectedPath(path)) return { kind: "keep" };

  if (/^\/(privacy|terms|security)$/.test(path)) return { kind: "keep" };
  if (path === EXPORT_PATH) return { kind: "export" };
  if (path === "/cli-auth") return { kind: "notice", reason: "cli-auth" };
  if (path.startsWith("/authorize/")) return { kind: "notice", reason: "authorize" };

  const share = path.match(/^\/view\/([0-9a-fA-F-]{8,64})$/);
  if (share) return { kind: "share", token: share[1] };
  const pub = path.match(/^\/@([a-z0-9][a-z0-9-]*[a-z0-9])\/([a-z0-9][a-z0-9-]*)$/);
  if (pub) return { kind: "share", username: pub[1], slug: pub[2] };
  if (/^\/@[a-z0-9][a-z0-9-]*[a-z0-9]$/.test(path)) return { kind: "notice", reason: "profile" };

  // /~<id> and legacy /d/<id> name a document in THIS browser's IndexedDB:
  // only the export page can get it out.
  if (/^\/(?:~|d\/)[A-Za-z0-9_-]{8,64}$/.test(path)) return { kind: "notice", reason: "local-doc" };

  const inline = inlineHandoff(loc);
  if (inline) return inline;

  return { kind: "notice", reason: "editor" };
}
