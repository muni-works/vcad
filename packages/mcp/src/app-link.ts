/**
 * Live link to a running vcad desktop app. Off the hosted path, each edit to
 * a session is pushed into the app's window as one undo step, so any MCP
 * client can drive the app with the full tool set. The app needs no runtime
 * of its own: this server already runs where the agent does.
 *
 * Every failure here is silent to the tool: a missing or busy app means the
 * server behaves exactly as it does without one. `VCAD_APP=0` opts out.
 */
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import type { Document } from "@vcad/ir";

type Link = { pushed: string; shown: boolean };
const links = new Map<string, Link>();
const refusedNoted = new Set<string>();

type Call = (tool: string, args: Record<string, unknown>) => Promise<Record<string, unknown> | null>;

function endpoint(): string | null {
  if (process.env.VCAD_APP === "0" || process.env.VERCEL) return null;
  try {
    const info = JSON.parse(readFileSync(join(homedir(), ".vcad", "app.json"), "utf8"));
    if (typeof info.pid === "number") process.kill(info.pid, 0);
    return info.url ?? `http://127.0.0.1:${info.port}/mcp`;
  } catch {
    return null;
  }
}

const callApp: Call = async (tool, args) => {
  const url = endpoint();
  if (!url) return null;
  try {
    const res = await fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 1,
        method: "tools/call",
        params: { name: tool, arguments: args },
      }),
      signal: AbortSignal.timeout(120_000),
    });
    const body = (await res.json()) as {
      result?: { content?: { text?: string }[]; isError?: boolean };
    };
    if (body.result?.isError) return null;
    return JSON.parse(body.result?.content?.[0]?.text ?? "null");
  } catch {
    return null;
  }
};

/**
 * Before a write: when the user has changed the linked document in the app,
 * take their version into the session and return why the call must stop.
 * Their work wins over a stale edit, and the app's node ids differ from ours.
 */
export async function reconcileBeforeWrite(
  documentId: string,
  documents: { set(id: string, doc: Document): unknown },
  call: Call = callApp,
): Promise<string | null> {
  const link = links.get(documentId);
  if (!link?.shown) return null;
  const status = await call("link_status", { link_id: documentId });
  if (!status?.linked || !status.edited) return null;
  const pulled = await call("link_status", { link_id: documentId, pull: true });
  if (!pulled?.document) return null;
  const document = pulled.document as Document;
  documents.set(documentId, document);
  link.pushed = JSON.stringify(document);
  return (
    "The user changed this document in vcad, so this edit was not applied. " +
    "The session now holds their version and node ids have changed: read it again with get_document, then retry."
  );
}

/** After a write: show the session in the app. Returns a note for the agent, once. */
export async function pushAfterWrite(
  documentId: string,
  tool: string,
  document: Document | undefined,
  call: Call = callApp,
): Promise<string | null> {
  if (!document) return null;
  const json = JSON.stringify(document);
  const link = links.get(documentId);
  if (link?.pushed === json) return null;
  const reply = await call("adopt_document", { document, link_id: documentId, tool });
  if (reply?.adopted) {
    const first = !link?.shown;
    links.set(documentId, { pushed: json, shown: true });
    return first ? "This document is live in the vcad app: the user sees each edit as one undo step." : null;
  }
  if (reply?.reason === "busy" && !refusedNoted.has(documentId)) {
    refusedNoted.add(documentId);
    return "The vcad app is open on other work, so this session isn't shown there.";
  }
  return null;
}

/** Test seam: forget every link. */
export function resetAppLinks(): void {
  links.clear();
  refusedNoted.clear();
}
