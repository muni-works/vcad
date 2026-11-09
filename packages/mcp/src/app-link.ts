/**
 * Live link to a running vcad desktop app. Off the hosted path, each edit to
 * a session is pushed into the app's window as one undo step, so any MCP
 * client can drive the app with the full tool set. The app needs no runtime
 * of its own: this server already runs where the agent does.
 *
 * A missing app is silent: the server behaves exactly as it does without one.
 * A window holding the user's work refuses the session, and every affected
 * call says so. `VCAD_APP=0` opts out.
 */
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, isAbsolute, join } from "node:path";
import type { Document } from "@vcad/ir";
import { documents, nextSessionId, runDocumentEnterHooks } from "./tools/session-core.js";
import { behavior, type ToolDef } from "./tools/tool-def.js";

/** `viewOnly`: a copy of a document the app can't edit; only poses mirror. */
type Link = { pushed: string; shown: boolean; viewOnly?: boolean };
const links = new Map<string, Link>();
const folders = new Map<string, string>();

/** Remember the folder a session's document came from, for its relative mesh paths. */
export function noteDocumentFolder(documentId: string, folder: string): void {
  folders.set(documentId, folder);
}

function documentFolder(documentId: string, document: Document): string | undefined {
  const known = folders.get(documentId);
  if (known) return known;
  const path = document.source?.path;
  return path && isAbsolute(path) ? dirname(path) : undefined;
}

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
  if (!link?.shown || link.viewOnly) return null;
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

/**
 * After a write: show the session in the app. Returns a note for the agent:
 * once when it goes live, and on every call the user's work keeps it off screen.
 */
export async function pushAfterWrite(
  documentId: string,
  tool: string,
  document: Document | undefined,
  call: Call = callApp,
): Promise<string | null> {
  if (!document) return null;
  const json = JSON.stringify(document);
  const link = links.get(documentId);
  if (link?.pushed === json || link?.viewOnly) return null;
  const base_dir = documentFolder(documentId, document);
  const reply = await call("adopt_document", {
    document,
    link_id: documentId,
    tool,
    ...(base_dir ? { base_dir } : {}),
  });
  if (reply?.adopted) {
    const first = !link?.shown;
    links.set(documentId, { pushed: json, shown: true });
    return first ? "This document is live in the vcad app: the user sees each edit as one undo step." : null;
  }
  if (reply?.reason === "busy") {
    return (
      "Not shown in the vcad app: its window holds the user's own work, which is never overwritten. " +
      "This edit exists only in this session. To work on their document, use attach_app_document."
    );
  }
  if (reply?.reason === "edited") {
    return "Not shown in the vcad app: the user just changed this document there. Your next edit will pick up their version.";
  }
  return null;
}

/** Why `attach_app_document` could not attach. */
export type AttachFailure = { error: string };
export type Attached = {
  document_id: string;
  name?: string;
  path?: string;
  parts: number;
  linked: boolean;
  note: string;
};

/**
 * Pull the document open in the app into a new session, linked so the session's
 * edits land there and the user's edits win as usual. The id is minted first
 * because the app records it as the link before returning the document.
 */
export async function attachAppDocument(
  call: Call = callApp,
  mintId: () => string = nextSessionId,
  store: (id: string, doc: Document) => void = (id, doc) => {
    documents.set(id, doc);
    runDocumentEnterHooks(doc);
  },
): Promise<Attached | AttachFailure> {
  const id = mintId();
  const reply = await call("attach_document", { link_id: id });
  if (!reply) return { error: "No running vcad app was found. Open the vcad desktop app, then try again." };
  if (!reply.attached || !reply.document) {
    return { error: "The vcad app has no document open: its window is an empty Untitled." };
  }
  const document = reply.document as Document;
  store(id, document);
  if (typeof reply.base_dir === "string") folders.set(id, reply.base_dir);
  const viewOnly = reply.view_only === true;
  links.set(id, { pushed: JSON.stringify(document), shown: true, viewOnly });
  return {
    document_id: id,
    ...(typeof reply.name === "string" ? { name: reply.name } : {}),
    ...(typeof reply.path === "string" ? { path: reply.path } : {}),
    parts: document.roots?.length ?? 0,
    linked: !viewOnly,
    note: viewOnly
      ? "This document is view-only in vcad (generated from a .loon file, or electronics), so this session is a copy: edits stay here, poses still mirror."
      : "Linked to the user's open document: each edit lands there as one undo step, and their edits win over a stale call.",
  };
}

export const toolDefs: ToolDef[] = [
  {
    name: "attach_app_document",
    pack: null,
    description:
      "Attach to the document the user has open in the vcad desktop app: pulls it into a new " +
      "session and links it, so each edit you make lands in their window as one undo step. " +
      "Use this when the user says \"this part\" or \"my document\" and has vcad open. " +
      "Takes no arguments; returns document_id.",
    inputSchema: { type: "object" as const, properties: {} },
    handler: async () => {
      const attached = await attachAppDocument();
      return "error" in attached
        ? { content: [{ type: "text" as const, text: attached.error }], isError: true }
        : { content: [{ type: "text" as const, text: JSON.stringify(attached) }] };
    },
    behavior: behavior({ writesDoc: true }),
  },
];

const POSE_INTERVAL_MS = 33;
let poseBusy = false;
let lastPose = 0;

/**
 * Mirror a simulation step in the app. Latest wins: steps that arrive while a
 * pose is in flight, or inside the frame interval, are dropped so a training
 * loop is never slowed by the display. Multi-DOF joints have no angle to set.
 */
export function pushPose(
  documentId: string,
  joints: { id: string; position: number; positions?: number[] }[],
  call: Call = callApp,
  now: number = Date.now(),
  base?: { pose: number[]; restart: boolean },
): boolean {
  if (!links.get(documentId)?.shown || poseBusy) return false;
  // A new episode's first pose anchors the base, so it is never dropped.
  if (!base?.restart && now - lastPose < POSE_INTERVAL_MS) return false;
  const pose = Object.fromEntries(joints.filter((j) => !j.positions).map((j) => [j.id, j.position]));
  if (Object.keys(pose).length === 0 && !base) return false;
  poseBusy = true;
  lastPose = now;
  const args = base ? { joints: pose, base: base.pose, restart: base.restart } : { joints: pose };
  void call("set_pose", args).finally(() => {
    poseBusy = false;
  });
  return true;
}

/** Test seam: forget every link. */
export function resetAppLinks(): void {
  links.clear();
  poseBusy = false;
  lastPose = 0;
  folders.clear();
}
