/**
 * Load documents from URL parameters.
 *
 * URL format: https://vcad.io/#/new?doc=<compressed>&name=<name>
 *
 * The `doc` parameter contains gzip-compressed, base64url-encoded VCode.
 *
 * Share links (/view/<token>, /@user/slug) were served by vcad.io's hosted
 * database and are gone; only self-contained URL documents load.
 */

import { parseVcadFile, type VcadFile } from "@vcad/core";
import type { ViewerState } from "@/lib/viewer-state";

/**
 * Base64url decode (URL-safe base64 without padding). Throws a
 * descriptive Error when the input is not a valid base64 string so that
 * the caller can render a user-friendly message instead of letting an
 * unhandled exception crash the page.
 */
function base64urlDecode(str: string): Uint8Array {
  // Restore standard base64
  let base64 = str.replace(/-/g, "+").replace(/_/g, "/");
  // Add padding
  while (base64.length % 4) {
    base64 += "=";
  }
  let binary: string;
  try {
    binary = atob(base64);
  } catch {
    throw new Error("Invalid base64url in shared document URL");
  }
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/**
 * Decompress gzip data using DecompressionStream (native browser API).
 */
async function decompressGzip(data: Uint8Array): Promise<string> {
  // Use Response + DecompressionStream for cleaner API
  // Cast to BlobPart to satisfy TypeScript's strict buffer type checking
  const blob = new Blob([data as unknown as BlobPart]);
  const ds = new DecompressionStream("gzip");
  const decompressedStream = blob.stream().pipeThrough(ds);
  const decompressedBlob = await new Response(decompressedStream).blob();
  return decompressedBlob.text();
}

export interface UrlDocumentParams {
  doc: string;
  name?: string;
  raw?: boolean; // If true, doc is raw VCode (not compressed)
}

/**
 * Parse URL hash parameters.
 * Expects format: #/new?doc=<compressed>&name=<name>
 * Also supports: ?ir=<url-encoded-vcode> for simple sharing
 */
export function parseUrlParams(): UrlDocumentParams | null {
  // Check for simple ?ir= parameter first (uncompressed, URL-encoded)
  const searchParams = new URLSearchParams(window.location.search);
  const rawIr = searchParams.get("ir");
  if (rawIr) {
    return {
      doc: rawIr,
      name: searchParams.get("name") ?? undefined,
      raw: true, // Flag to skip decompression
    };
  }

  // Check for hash-based compressed format
  const hash = window.location.hash;
  if (!hash.startsWith("#/new?")) {
    return null;
  }

  const queryString = hash.slice(6); // Remove "#/new?"
  const params = new URLSearchParams(queryString);

  const doc = params.get("doc");
  if (!doc) {
    return null;
  }

  return {
    doc,
    name: params.get("name") ?? undefined,
  };
}

export interface UrlDocumentResult {
  file: VcadFile;
  name: string;
  /** Present when the doc was loaded from a share/public link (read-only). */
  readOnlyShareToken?: string;
  /** Present when the share URL carried a ?at=<encoded> viewer-state hint. */
  viewerStateHint?: ViewerState;
  /** When viewing a public doc via /@user/slug, the owner info. */
  ownerUsername?: string;
}

/** Legal page slugs served from the SPA. */
export type LegalSlug = "privacy" | "terms" | "security";

/** Route type detected from the current pathname. */
type UrlRoute =
  | { kind: "local-doc"; id: string }
  | { kind: "legal"; slug: LegalSlug }
  | null;

/** Parse the current pathname into a route. */
function parseRoute(): UrlRoute {
  if (typeof window === "undefined") return null;
  const path = window.location.pathname;

  // /privacy, /terms, /security
  const legalMatch = path.match(/^\/(privacy|terms|security)\/?$/);
  if (legalMatch?.[1]) return { kind: "legal", slug: legalMatch[1] as LegalSlug };

  // /~<id> — local IndexedDB document identity (canonical). The legacy
  // /d/<id> form is still accepted so existing bookmarks keep working.
  // Accepts both legacy UUIDs (hex + dashes) and short base62 nanoids
  // minted by newDocId().
  const localMatch = path.match(/^\/(?:~|d\/)([A-Za-z0-9_-]{8,64})\/?$/);
  if (localMatch?.[1]) return { kind: "local-doc", id: localMatch[1] };

  return null;
}

/**
 * Returns the local document ID from the current URL, if the user landed on
 * a `/~<id>` (or legacy `/d/<id>`) path. Bootstrap checks this before falling back to
 * "most recent document" so refresh always reopens the exact doc.
 */
export function getLocalDocRouteId(): string | null {
  const route = parseRoute();
  return route?.kind === "local-doc" ? route.id : null;
}

/**
 * Load a document from URL parameters.
 * Returns null if no URL document is present or loading fails.
 */
export async function loadDocumentFromUrl(): Promise<UrlDocumentResult | null> {
  // Hash-based VCode shares + ?ir= raw shares
  const params = parseUrlParams();
  if (!params) {
    return null;
  }

  try {
    let compact: string;

    if (params.raw) {
      // Raw VCode (URL-decoded by URLSearchParams)
      compact = params.doc;
    } else {
      // Decode and decompress base64url + gzip
      const compressed = base64urlDecode(params.doc);
      compact = await decompressGzip(compressed);
    }

    // Parse VCode into VcadFile
    const file = parseVcadFile(compact);

    // Clear the URL to prevent re-loading on refresh
    window.history.replaceState(null, "", window.location.pathname);

    return {
      file,
      name: params.name ?? "Shared Document",
    };
  } catch (err) {
    console.error("Failed to load document from URL:", err);
    return null;
  }
}

/**
 * Check if the current URL has document parameters.
 */
export function hasUrlDocument(): boolean {
  return parseUrlParams() !== null || parseRoute() !== null;
}

/**
 * Check if the current URL is a legal page (/privacy, /terms, /security).
 * The app renders a LegalPage instead of the normal editor in this case.
 */
export function getLegalRouteSlug(): LegalSlug | null {
  const route = parseRoute();
  return route?.kind === "legal" ? route.slug : null;
}
