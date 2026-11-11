/**
 * continue_document — open an MCP editing session from an inline "Continue in
 * Claude" handoff (gzip + base64url of the IR, or VCode). CRDT content is
 * materialized through the kernel engine's `WasmDocumentEngine.load()`.
 *
 * The hosted share-token path (resolved against vcad.io's Supabase
 * `documents` table) was removed with the hosted services; a `token` now
 * fails closed with a clear error.
 */
import { getKernelWasm } from "@vcad/engine";
import { fromVCode } from "@vcad/ir";
import type { Document } from "@vcad/ir";
import { gunzipSync } from "node:zlib";
import { registerSession } from "./session.js";
import { behavior, type ToolDef } from "./tool-def.js";
import { ok as okJson, errText as err, type ToolResult } from "./tool-result.js";

export const continueDocumentSchema = {
  type: "object" as const,
  properties: {
    token: {
      type: "string" as const,
      description:
        "Legacy hosted share token. Not available in the local build (hosted " +
        "services were retired) — returns an error; use `doc`.",
    },
    doc: {
      type: "string" as const,
      description:
        "An inline, compressed document handoff (gzip + base64url of the IR), " +
        "used when the part was handed off without a cloud account. Supply " +
        "either `token` or `doc`.",
    },
  },
};

type ToolText = ToolResult;

/** The success payload: the session handle plus a nudge toward the verify loop
 *  (render it, then leave a re-runnable Receipt for any claim). */
function ok(id: string, doc: Document, name: string): ToolText {
  return okJson({
    document_id: id,
    parts: doc.roots?.length ?? 0,
    name,
    hint:
      "Render with render_view, then continue the user's work. Leave a " +
      "re-runnable proof of any claim with verify_part / build_receipt.",
  });
}

function looksLikeIr(c: unknown): c is Document {
  return (
    !!c &&
    typeof c === "object" &&
    "nodes" in (c as object) &&
    "roots" in (c as object)
  );
}

function looksLikeCrdt(c: unknown): boolean {
  return (
    !!c &&
    typeof c === "object" &&
    "replica_id" in (c as object) &&
    Array.isArray((c as { ops?: unknown }).ops)
  );
}

/** Decode an inline `doc` handoff (gzip + base64url of VCode or IR JSON) into
 *  raw content for {@link materialize}. Mirrors the encoding open_in_app /
 *  the web app produce. Throws on a corrupt blob — the caller turns that into a
 *  re-share hint. */
function decodeInlineDoc(blob: string): unknown {
  const b64 = blob.replace(/-/g, "+").replace(/_/g, "/");
  const buf = Buffer.from(b64, "base64");
  const text = gunzipSync(buf).toString("utf-8").trim();
  // The web app gzips raw IR JSON; open_in_app may emit VCode (`#…`).
  return text.startsWith("#") ? fromVCode(text) : JSON.parse(text);
}

/** Materialize raw cloud `content` into an IR Document. Returns null for shapes
 *  this build can't open (e.g. loon-only, or CRDT when the engine is absent). */
async function materialize(content: unknown): Promise<Document | null> {
  // Already IR — defensive copy so the caller can't mutate a shared reference.
  if (looksLikeIr(content)) {
    return JSON.parse(JSON.stringify(content)) as Document;
  }
  if (looksLikeCrdt(content)) {
    const wasm = (await getKernelWasm()) as unknown as {
      WasmDocumentEngine?: {
        load(bytes: Uint8Array): { get_document_json(): string };
      };
    };
    const Engine = wasm?.WasmDocumentEngine;
    if (!Engine) return null;
    // The web app stores CRDT as JSON; `crdtBytes` is its UTF-8 encoding
    // (see cloudContentToVcadFile). Mirror that so `load()` accepts it.
    const bytes = new TextEncoder().encode(JSON.stringify(content));
    const engine = Engine.load(bytes);
    return JSON.parse(engine.get_document_json()) as Document;
  }
  return null;
}

export async function continueDocument(
  args: Record<string, unknown>,
): Promise<ToolText> {
  const token = String(args.token ?? "").trim();
  const inlineDoc = String(args.doc ?? "").trim();
  if (!token && !inlineDoc) {
    return err(
      "continue_document requires an inline `doc` from a Continue in Claude " +
        "handoff.",
    );
  }

  // Share tokens resolved against the hosted Supabase `documents` table,
  // which the local build does not have. Fail closed with a clear message.
  if (token) {
    return err(
      "continue_document `token` handoffs are not available in the local build " +
        "of vcad MCP (hosted services were retired in vcad 1.0). Pass an inline " +
        "`doc`, or open the .vcad file with load_document.",
    );
  }

  // ── Accountless inline handoff: no stable key, so a fresh session id.
  let content: unknown;
  try {
    content = decodeInlineDoc(inlineDoc);
  } catch (e) {
    return err(
      `Couldn't read the inline handoff: ${(e as Error).message}. ` +
        `Ask the user for a fresh handoff.`,
    );
  }
  let doc: Document | null;
  try {
    doc = await materialize(content);
  } catch (e) {
    return err(
      `Could not open the shared document: ${(e as Error).message}. ` +
        `Ask the user for a fresh handoff.`,
    );
  }
  if (!doc) {
    return err(
      "The shared document is in a format this build can't open yet " +
        "(expected CRDT or IR content).",
    );
  }
  return ok(registerSession(doc), doc, "Shared part");
}

export const toolDefs: ToolDef[] = [
  {
    name: "continue_document",
    pack: null,
    description:
      "Open a part from an inline 'Continue in Claude' handoff (`doc`: gzip + " +
      "base64url IR) as an editing session, then render_view it and continue " +
      "the user's work. Returns a `document_id`. Share-token (`token`) " +
      "handoffs needed the retired hosted service and return an error.",
    inputSchema: continueDocumentSchema,
    handler: (a) => continueDocument(a),
    behavior: behavior({ writesDoc: true, geometry: true, mount: true }),
    annotations: {
      title: "Continue Handoff",
      readOnlyHint: true,
      destructiveHint: false,
      idempotentHint: false,
      openWorldHint: false,
    },
  },
];
