/**
 * open_in_app — hand a document to the native vcad app.
 *
 * vcad 1.0 is a native Mac app; the vcad.io web editor is retired. The tool
 * emits a `vcad://` deep link (format: docs/launch/vcad-url-scheme.md) that the
 * app registers in its bundle Info.plist, plus the download page as the
 * fallback for machines without the app. The old `open_in_browser` tool (a
 * vcad.io web-editor link) was removed with the hosted services.
 */

import type { Document } from "@vcad/ir";
import type { Engine } from "@vcad/engine";
import { fromVCode } from "@vcad/ir";
import { gzipSync } from "node:zlib";
import { behavior, type ToolDef } from "./tool-def.js";
import { getSession } from "./session-core.js";
import { sessionDir, useFileSessionStore } from "../session-store.js";
import * as os from "node:os";
import * as nodePath from "node:path";

/** Where users without the native app get it. */
export const VCAD_DOWNLOAD_URL = "https://github.com/muni-works/vcad/releases/latest";

/** Session ids the app will look up on disk: no separators, no dots. */
const SESSION_ID_RE = /^[A-Za-z0-9_-]{1,128}$/;

interface ShareInput {
  document_id?: string; // live session — the session-first path
  document?: string; // inline JSON string or VCode — stateless path
  name?: string;
}

export const openInAppSchema = {
  type: "object" as const,
  properties: {
    document_id: {
      type: "string" as const,
      description:
        "Live session id (from open_document / create_cad_loon) to share — " +
        "the session-first path. Either this or `document` is required.",
    },
    document: {
      type: "string" as const,
      description:
        "Inline IR document (JSON or VCode format) — the stateless " +
        "alternative to document_id.",
    },
    name: {
      type: "string" as const,
      description: "Optional document name",
    },
  },
};

/**
 * Base64url encode (URL-safe base64 without padding).
 */
function base64urlEncode(data: Buffer): string {
  return data
    .toString("base64")
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

/**
 * Parse a document from JSON or VCode format.
 */
function parseDocument(input: string): Document {
  const trimmed = input.trim();
  if (trimmed.startsWith("{")) {
    return JSON.parse(trimmed) as Document;
  }
  if (trimmed.startsWith("#")) {
    return fromVCode(trimmed);
  }
  throw new Error("Document must be JSON (starting with '{') or VCode (starting with '#')");
}

/**
 * Compress a VCode document for URL embedding.
 * Format: gzip + base64url
 */
function compressForUrl(compact: string): string {
  const compressed = gzipSync(Buffer.from(compact, "utf-8"), { level: 9 });
  return base64urlEncode(compressed);
}

export interface AppLink {
  /** `vcad://open?...` — opens the document in the native app. */
  url: string;
  /** How the document travels: a session id on the shared disk store, or inline. */
  via: "session" | "inline";
}

/**
 * Build the `vcad://` deep link for a document.
 *
 * A live session on the local disk store (`~/.vcad/mcp-sessions`, the default
 * for local MCP runs) travels by id — the app reads `<id>.json` from the same
 * directory, so the link stays short however big the model is. Anything else
 * (inline document, disk store off) is embedded as gzip+base64url JSON. JSON
 * rather than VCode: the native app loads `.vcad` JSON directly.
 */
export function buildAppLink(
  doc: Document,
  opts: { documentId?: string; name?: string; fileStore?: boolean } = {},
): AppLink {
  const params = new URLSearchParams();
  const fileStore = opts.fileStore ?? useFileSessionStore();
  let via: AppLink["via"];
  if (opts.documentId && fileStore && SESSION_ID_RE.test(opts.documentId)) {
    params.set("session", opts.documentId);
    const dir = sessionDir();
    const defaultDir = nodePath.join(os.homedir(), ".vcad", "mcp-sessions");
    if (nodePath.resolve(dir) !== nodePath.resolve(defaultDir)) {
      params.set("dir", dir);
    }
    via = "session";
  } else {
    params.set("doc", compressForUrl(JSON.stringify(doc)));
    via = "inline";
  }
  if (opts.name) params.set("name", opts.name);
  return { url: `vcad://open?${params.toString()}`, via };
}

export function openInApp(
  input: unknown,
  engine?: Engine,
): { content: Array<{ type: "text"; text: string }> } {
  const { document_id, document: docInput, name } = input as ShareInput;

  // Session-first: resolve a live session by id. Inline `document` remains
  // the stateless escape hatch.
  if (!document_id && !docInput) {
    throw new Error(
      "Pass `document_id` (a live session) or `document` (inline IR).",
    );
  }
  const live = document_id ? getSession(document_id) : parseDocument(docInput!);
  // A `step_import` node is a reference into this server's STEP registry — the
  // app cannot resolve it and would show an empty part. Bake those nodes to
  // meshes for the trip; when baking changed the document, it must travel
  // inline (the on-disk session still holds the reference).
  const doc = engine ? engine.bakeStepImports(live) : live;
  const baked = doc !== live;
  const link = buildAppLink(doc, {
    documentId: baked ? undefined : document_id,
    name,
  });

  const lines = [
    `Open in vcad:\n${link.url}`,
    "",
    `Don't have the vcad app? Download it: ${VCAD_DOWNLOAD_URL}`,
    link.via === "session"
      ? `(Link carries session id ${document_id}; the app reads it from this machine's session store, so it works on the machine running this MCP server.)`
      : `(Document embedded in the link: ${link.url.length} characters.)`,
  ];
  return { content: [{ type: "text", text: lines.join("\n") }] };
}

const DESCRIPTION =
  "Open a CAD document in the native vcad app via a `vcad://` deep link. " +
  "Pass a live session's `document_id` (the usual ship path), or an inline IR `document` (JSON or VCode format). " +
  "Local sessions travel by id; otherwise the document is embedded (gzip + base64url). " +
  "The result also carries the vcad download page as the fallback for users without the app.";

export const toolDefs: ToolDef[] = [
  {
    name: "open_in_app",
    pack: null,
    description: DESCRIPTION,
    inputSchema: openInAppSchema,
    handler: (a, c) => openInApp(a, c.engine),
    behavior: behavior({}),
  },
];
