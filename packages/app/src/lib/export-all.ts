/**
 * Export-all for the vcad.io retirement: every cloud document the signed-in
 * user owns → one zip of `.vcad` files.
 *
 * Read-only against the existing `documents` table (RLS already scopes rows to
 * the caller) — no new tables, no migrations, nothing written back. The
 * fetcher is injected so the zip logic is testable without Supabase and the
 * page can be previewed against a mock session in dev.
 */

import { zipSync, strToU8 } from "fflate";

/** One cloud document with its full content, as fetched for export. */
export interface ExportableDocument {
  id: string;
  name: string;
  content: unknown;
  updated_at: string;
}

/** Fetch one page of documents (offset/limit), newest first. */
export type DocumentPageFetcher = (
  offset: number,
  limit: number,
) => Promise<ExportableDocument[]>;

/** Rows per round trip. Content is the whole model, so keep pages small. */
export const EXPORT_PAGE_SIZE = 25;

/** Supabase-backed fetcher over the existing `documents` table. */
export function supabaseDocumentFetcher(
  // Narrow structural type so this module doesn't pull the Supabase types in.
  supabase: {
    from: (table: string) => {
      select: (cols: string) => {
        order: (
          col: string,
          opts: { ascending: boolean },
        ) => {
          range: (
            from: number,
            to: number,
          ) => PromiseLike<{ data: unknown; error: unknown }>;
        };
      };
    };
  },
): DocumentPageFetcher {
  return async (offset, limit) => {
    const { data, error } = await supabase
      .from("documents")
      .select("id, name, content, updated_at")
      .order("updated_at", { ascending: false })
      .range(offset, offset + limit - 1);
    if (error) throw error;
    return (data ?? []) as ExportableDocument[];
  };
}

/** Pull every document, page by page, reporting progress. */
export async function fetchAllDocuments(
  fetchPage: DocumentPageFetcher,
  onProgress?: (fetched: number) => void,
  pageSize = EXPORT_PAGE_SIZE,
): Promise<ExportableDocument[]> {
  const all: ExportableDocument[] = [];
  for (;;) {
    const page = await fetchPage(all.length, pageSize);
    all.push(...page);
    onProgress?.(all.length);
    if (page.length < pageSize) return all;
  }
}

/** A filesystem-safe base name; never empty. */
export function safeFileName(name: string): string {
  const cleaned = name
    // eslint-disable-next-line no-control-regex -- stripping control chars is the point
    .replace(/[\\/:*?"<>|\u0000-\u001f]/g, "_")
    .replace(/^\.+/, "")
    .trim()
    .slice(0, 120);
  return cleaned || "Untitled";
}

/**
 * Cloud `content` → the bytes of the file the app would have saved.
 *
 * `documents.content` already holds the on-disk payload (see
 * `vcadFileToCloudContent` below): CRDT JSON (a v0.4 `.vcad`), a raw IR
 * Document (a v0.1 `.vcad`), or a `{loonSource}` envelope — which unwraps to a
 * `.loon` source file, the form both the web and native apps open directly.
 */
export function cloudContentToFile(content: unknown): { ext: "vcad" | "loon"; text: string } {
  if (content && typeof content === "object") {
    const loon = (content as Record<string, unknown>).loonSource;
    if (typeof loon === "string") return { ext: "loon", text: loon };
  }
  return { ext: "vcad", text: JSON.stringify(content ?? null) };
}

/**
 * A local (IndexedDB) `VcadFile` → the same payload the old sync wrote to
 * `documents.content`, so browser documents export like cloud ones:
 * CRDT JSON, a `{loonSource}` envelope, or the raw IR Document.
 */
export function vcadFileToCloudContent(document: unknown): unknown {
  if (!document || typeof document !== "object") return document;
  const obj = document as Record<string, unknown>;
  if (obj.kind === "crdt") {
    const bytes = obj.crdtBytes;
    // IDB without typed-array support stores a plain number array.
    const u8 = bytes instanceof Uint8Array ? bytes : Array.isArray(bytes) ? new Uint8Array(bytes as number[]) : null;
    if (!u8) return null;
    try {
      return JSON.parse(new TextDecoder().decode(u8));
    } catch {
      return null;
    }
  }
  if (obj.kind === "loon") return { loonSource: obj.loonSource };
  if (obj.kind === "legacy") return obj.document ?? null;
  // Untagged shape from before the tagged-union migration.
  if (typeof obj.document === "object") return obj.document;
  return document;
}

/**
 * Build the zip: one file per document (`<name>.vcad`, or `.loon` for loon
 * documents; deduplicated with ` (2)`, ` (3)`), plus a `manifest.json` mapping
 * each file to its cloud id and last-modified time.
 */
export function buildExportZip(
  docs: ExportableDocument[],
  exportedAt = new Date(),
): Uint8Array {
  const files: Record<string, Uint8Array> = {};
  const used = new Set<string>();
  const manifest: Array<{ file: string; id: string; name: string; updated_at: string }> = [];

  for (const doc of docs) {
    const base = safeFileName(doc.name);
    const { ext, text } = cloudContentToFile(doc.content);
    let file = `${base}.${ext}`;
    for (let n = 2; used.has(file.toLowerCase()); n++) file = `${base} (${n}).${ext}`;
    used.add(file.toLowerCase());
    files[file] = strToU8(text);
    manifest.push({ file, id: doc.id, name: doc.name, updated_at: doc.updated_at });
  }

  files["manifest.json"] = strToU8(
    JSON.stringify({ exported_at: exportedAt.toISOString(), count: docs.length, documents: manifest }, null, 2),
  );
  return zipSync(files, { level: 6 });
}

/** `vcad-export-YYYY-MM-DD.zip` */
export function exportZipName(date = new Date()): string {
  return `vcad-export-${date.toISOString().slice(0, 10)}.zip`;
}
