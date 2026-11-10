/**
 * Artifact store — keep large tool outputs OUT of model context.
 *
 * Export tools (export_gerber, export_cad, import_step) can produce payloads
 * far larger than an MCP tool result should carry: a Gerber fab bundle is
 * ~11–13 files / ~168 KB, which blows the model's tool-output token budget if
 * returned inline and is unusable through the channel. The byte-cap idiom
 * (see maxInlineArtifactBytes / maxInlineExportBytes in remote.ts) decides per
 * call — small payloads stay inline; large ones are written HERE and the tool
 * returns a compact { artifact_url, manifest } handle instead. The bytes then
 * ride the artifact channel (the /artifacts/<id> HTTP route), never the model's
 * context. quote_manufacturing accepts the same handle, so a
 * quote references the bundle without ever re-sending the files.
 *
 * Backing: an in-process, bounded, TTL'd registry. The local build has no
 * durable artifact backend — handles live as long as the server process (the
 * hosted Supabase `mcp_artifacts` table was retired in vcad 1.0).
 */

import { createHash, randomBytes } from "node:crypto";

/** One file handed to the store (Gerber text, an STL/GLB binary, an IR doc). */
export interface ArtifactInputFile {
  name: string;
  content: string | Uint8Array;
  /** Optional MIME type; inferred from the extension when omitted. */
  contentType?: string;
}

/** One manifest row: the file, its size, and a content hash for verification. */
export interface ManifestEntry {
  file: string;
  bytes: number;
  sha256: string;
}

/** The compact handle a tool returns in place of inline bytes. */
export interface ArtifactHandle {
  artifact_id: string;
  /** Index URL — GETs the manifest; `${artifact_url}/<file>` GETs one file. */
  artifact_url: string;
  manifest: ManifestEntry[];
  /** Total bundle size across all files. */
  bytes: number;
  expires_at: string;
}

interface StoredFile {
  name: string;
  buf: Buffer;
  contentType: string;
}

interface StoredArtifact {
  id: string;
  files: StoredFile[];
  manifest: ManifestEntry[];
  bytes: number;
  createdAt: number;
  expiresAt: number;
}

// Module-global registry — survives across tool calls on one warm instance
// (same lifetime model as the in-memory session/order maps).
const registry = new Map<string, StoredArtifact>();

function ttlMs(): number {
  const raw = process.env.MCP_ARTIFACT_TTL_MS;
  const n = raw ? parseInt(raw, 10) : NaN;
  return Number.isFinite(n) && n > 0 ? n : 24 * 60 * 60 * 1000; // 24h
}

function maxEntries(): number {
  const raw = process.env.MCP_ARTIFACT_MAX_ENTRIES;
  const n = raw ? parseInt(raw, 10) : NaN;
  return Number.isFinite(n) && n > 0 ? n : 256;
}

/** Drop expired entries, then bound the registry to maxEntries (oldest-first;
 *  Map preserves insertion order). Keeps a long-running server from growing
 *  without limit. */
function evict(now: number): void {
  for (const [id, a] of registry) {
    if (a.expiresAt <= now) registry.delete(id);
  }
  while (registry.size >= maxEntries()) {
    const oldest = registry.keys().next().value;
    if (oldest === undefined) break;
    registry.delete(oldest);
  }
}

/** Base URL for artifact links: `VCAD_MCP_PUBLIC_URL` (the local HTTP
 *  transport's own origin) when set, else empty → links stay relative
 *  (`/artifacts/<id>`), which trust-boundary and parseArtifactId both accept. */
function publicBaseUrl(): string {
  const raw = process.env.VCAD_MCP_PUBLIC_URL;
  if (!raw) return "";
  try {
    return new URL(raw).origin;
  } catch {
    return "";
  }
}

/** The capability URL for an artifact id (possession of the id is the grant). */
export function artifactUrl(id: string): string {
  return `${publicBaseUrl()}/artifacts/${encodeURIComponent(id)}`;
}

/** The URL for a single file within an artifact. */
export function artifactFileUrl(id: string, file: string): string {
  return `${artifactUrl(id)}/${encodeURIComponent(file)}`;
}

function toBuffer(content: string | Uint8Array): Buffer {
  return typeof content === "string"
    ? Buffer.from(content, "utf8")
    : Buffer.from(content);
}

function guessContentType(name: string): string {
  const ext = name.toLowerCase().split(".").pop() ?? "";
  switch (ext) {
    case "stl":
      return "model/stl";
    case "glb":
      return "model/gltf-binary";
    case "step":
    case "stp":
      return "application/step";
    case "json":
    case "vcad":
      return "application/json";
    case "csv":
      return "text/csv";
    case "gbr":
    case "gbl":
    case "gtl":
    case "gto":
    case "gbs":
    case "gts":
    case "gko":
      return "application/vnd.gerber";
    case "drl":
    case "txt":
      return "text/plain";
    case "zip":
      return "application/zip";
    case "png":
      return "image/png";
    case "svg":
      return "image/svg+xml";
    case "gif":
      return "image/gif";
    default:
      return "application/octet-stream";
  }
}

/** Total byte count of a bundle — the value compared against the inline cap. */
export function bundleBytes(files: ArtifactInputFile[]): number {
  return files.reduce((n, f) => n + toBuffer(f.content).length, 0);
}

/** Build a manifest (file, bytes, sha256) without storing anything. */
export function buildManifest(files: ArtifactInputFile[]): ManifestEntry[] {
  return files.map((f) => {
    const buf = toBuffer(f.content);
    return {
      file: f.name,
      bytes: buf.length,
      sha256: createHash("sha256").update(buf).digest("hex"),
    };
  });
}

function newArtifactId(): string {
  // Unguessable — the id IS the capability to read the bundle (same model as
  // session ids).
  return `art_${randomBytes(12).toString("base64url")}`;
}

/** Store a bundle and return its handle (id, url, manifest). */
export function storeArtifact(files: ArtifactInputFile[]): ArtifactHandle {
  const now = Date.now();
  evict(now);
  const id = newArtifactId();
  const stored: StoredFile[] = files.map((f) => ({
    name: f.name,
    buf: toBuffer(f.content),
    contentType: f.contentType ?? guessContentType(f.name),
  }));
  const manifest = stored.map((f) => ({
    file: f.name,
    bytes: f.buf.length,
    sha256: createHash("sha256").update(f.buf).digest("hex"),
  }));
  const bytes = stored.reduce((n, f) => n + f.buf.length, 0);
  const expiresAt = now + ttlMs();
  const artifact: StoredArtifact = {
    id,
    files: stored,
    manifest,
    bytes,
    createdAt: now,
    expiresAt,
  };
  registry.set(id, artifact);
  return {
    artifact_id: id,
    artifact_url: artifactUrl(id),
    manifest,
    bytes,
    expires_at: new Date(expiresAt).toISOString(),
  };
}

/** Read a stored artifact by id (expired → absent). */
export function getArtifact(id: string): StoredArtifact | null {
  const a = registry.get(id);
  if (!a) return null;
  if (a.expiresAt <= Date.now()) {
    registry.delete(id);
    return null;
  }
  return a;
}

/** Read one file from a stored artifact . */
export function getArtifactFile(id: string, name: string): StoredFile | null {
  const a = getArtifact(id);
  if (!a) return null;
  return a.files.find((f) => f.name === name) ?? null;
}

/** The local build never has a durable artifact store. */
export function isArtifactStoreDurable(): boolean {
  return false;
}

/** Durability state for server_info / the /health endpoint. */
export function artifactStoreInfo(): { artifact_store: "in-memory" } {
  return { artifact_store: "in-memory" };
}

/** Async form of getArtifact, kept for the /artifacts route and quote tools. */
export async function getArtifactAsync(id: string): Promise<StoredArtifact | null> {
  return getArtifact(id);
}

/** Read one file from an artifact . */
export async function getArtifactFileAsync(
  id: string,
  name: string,
): Promise<StoredFile | null> {
  const a = await getArtifactAsync(id);
  if (!a) return null;
  return a.files.find((f) => f.name === name) ?? null;
}

/**
 * Resolve a caller-supplied artifact handle — accepts a raw id or a full
 * artifact_url (…/artifacts/<id>[/<file>]) — to the stored bundle, or null if
 * unknown/expired. Used by quote_manufacturing so the fab files
 * are fetched from the store, never re-sent through model context.
 */
export function resolveArtifact(handle: string): StoredArtifact | null {
  const id = parseArtifactId(handle);
  return id ? getArtifact(id) : null;
}

/** Async form of resolveArtifact. */
export async function resolveArtifactAsync(
  handle: string,
): Promise<StoredArtifact | null> {
  const id = parseArtifactId(handle);
  return id ? getArtifactAsync(id) : null;
}

/** A persistable reference to a stored bundle — metadata only, never bytes. */
export interface ArtifactRef {
  artifact_id: string;
  artifact_url: string;
  bytes: number;
  manifest: ManifestEntry[];
}

/** Resolve a handle to a metadata-only ref suitable for attaching to an order
 *  (the fab bytes stay in the store; only id/url/manifest travel). */
export function resolveArtifactRef(handle: string): ArtifactRef | null {
  const a = resolveArtifact(handle);
  if (!a) return null;
  return {
    artifact_id: a.id,
    artifact_url: artifactUrl(a.id),
    bytes: a.bytes,
    manifest: a.manifest,
  };
}

/** Async form of resolveArtifactRef (used by quote_manufacturing). */
export async function resolveArtifactRefAsync(
  handle: string,
): Promise<ArtifactRef | null> {
  const a = await resolveArtifactAsync(handle);
  if (!a) return null;
  return {
    artifact_id: a.id,
    artifact_url: artifactUrl(a.id),
    bytes: a.bytes,
    manifest: a.manifest,
  };
}

/** Extract the artifact id from a raw id or an artifact URL. */
export function parseArtifactId(handle: string): string | null {
  if (!handle) return null;
  if (registry.has(handle)) return handle;
  try {
    const parts = new URL(handle).pathname.split("/").filter(Boolean);
    const i = parts.indexOf("artifacts");
    if (i >= 0 && parts[i + 1]) return decodeURIComponent(parts[i + 1]);
  } catch {
    // not a URL — fall through
  }
  // Bare "art_…" id or a "/artifacts/<id>" path fragment.
  const seg = handle.split("/").filter(Boolean);
  const ai = seg.indexOf("artifacts");
  if (ai >= 0 && seg[ai + 1]) return decodeURIComponent(seg[ai + 1]);
  return handle.startsWith("art_") ? handle : null;
}

/** Test seam: clear the registry between cases. */
export function clearArtifacts(): void {
  registry.clear();
}
