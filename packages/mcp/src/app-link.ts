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
import { dirname, isAbsolute } from "node:path";
import type { Document } from "@vcad/ir";
import { describeInstance, listInstances, selectInstance, type InstanceRecord } from "./app-discovery.js";
import { documents, nextSessionId, runDocumentEnterHooks } from "./tools/session-core.js";
import { behavior, type ToolDef } from "./tools/tool-def.js";

/**
 * `viewOnly`: a copy of a document the app can't edit; only poses mirror.
 * `opened`: the session was deliberately opened, so it may take a window whose
 * session has gone idle. `lost`: why the window went away, told once.
 */
type Link = { pushed: string; shown: boolean; viewOnly?: boolean; opened?: boolean; lost?: string };
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

/**
 * The app this server is linked to, pinned for the process: a window launched
 * later never steals the link. When the pinned app exits and a different one
 * is picked up, every shown session is told the app changed instead of
 * silently pushing into a stranger's window. See app-discovery.ts for the
 * VCAD_APP_URL / VCAD_APP_PID / VCAD_APP_DOCUMENT selection.
 */
let pinned: { url: string; instance: InstanceRecord | null } | null = null;
let warned = "";

function endpoint(): string | null {
  if (process.env.VCAD_APP === "0") return null;
  const instances = listInstances();
  if (pinned && (process.env.VCAD_APP_URL || instances.some((r) => r.pid === pinned?.instance?.pid))) {
    return pinned.url;
  }
  const pick = selectInstance({ instances });
  if (pick.error !== undefined) return null;
  if (pick.warning && pick.warning !== warned) {
    warned = pick.warning;
    process.stderr.write(`[vcad-mcp] ${pick.warning}\n`);
  }
  const prev = pinned?.instance;
  if (prev && pick.instance && prev.instance_id !== pick.instance.instance_id) {
    appChanged(prev, pick.instance);
  }
  pinned = { url: pick.url, instance: pick.instance };
  return pick.url;
}

function appChanged(prev: InstanceRecord, next: InstanceRecord): void {
  const why =
    `The vcad app changed: ${describeInstance(prev)} is gone, now linked to ${describeInstance(next)} ` +
    "(it relaunched, or another window took over). Edits stay in this session. To show it there, call show_in_app.";
  for (const id of links.keys()) loseWindow(id, why);
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
  // The app is busy with an earlier push, and it re-checks for user edits
  // itself on every adopt, so a push made now is refused rather than lost.
  if (pushes.get(documentId)?.running) return null;
  const status = await Promise.race([
    call("link_status", { link_id: documentId }),
    wait(RECONCILE_WAIT_MS).then(() => null),
  ]);
  if (status?.linked === false) {
    loseWindow(documentId, LOST_WINDOW);
    return null;
  }
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
 * How long a tool call waits on the app. The window's re-solve runs on its
 * main thread and can take seconds; the agent's call must not. Past this, the
 * window catches up on its own and anything it has to say arrives with the
 * agent's next call.
 */
const PUSH_WAIT_MS = 100;
const RECONCILE_WAIT_MS = 150;
const wait = (ms: number) => new Promise<void>((done) => setTimeout(done, ms));

type Push = { run: () => Promise<string | null>; json: string; done: (note: string | null) => void };
/** Per session: one push in flight, and only the newest one waiting behind it. */
const pushes = new Map<string, { running: boolean; next?: Push; lastJson?: string }>();
/** Notes from pushes that finished after their tool call had already returned. */
const lateNotes = new Map<string, string>();

const LOST_WINDOW =
  "This document is no longer on screen in the vcad app: its window now shows something else. " +
  "Edits stay in this session. To put it back, call show_in_app.";

function loseWindow(documentId: string, why: string): void {
  const link = links.get(documentId);
  if (!link) return;
  link.shown = false;
  link.pushed = "";
  link.lost = why;
}

/** The note owed to a session that lost its window, once. Nothing if it is back on screen. */
export function takeLinkNote(documentId: string): string | null {
  const late = lateNotes.get(documentId);
  if (late) {
    lateNotes.delete(documentId);
    return late;
  }
  const link = links.get(documentId);
  if (!link?.lost) return null;
  const note = link.lost;
  delete link.lost;
  return link.shown ? null : note;
}

/**
 * How much a push may displace another session's window:
 * `implicit` never; `opened` once that session has gone idle; `explicit` always.
 * None of them can replace the user's own work.
 */
export type LinkIntent = "implicit" | "opened" | "explicit";

const OPENING_TOOLS = new Set(["open_document", "create_schematic", "create_cad_loon"]);

/** A session is deliberately opened only by these tools, and only when they mint it. */
export function linkIntent(tool: string, minted: boolean): LinkIntent {
  return minted && OPENING_TOOLS.has(tool) ? "opened" : "implicit";
}

async function drainPushes(documentId: string): Promise<void> {
  const state = pushes.get(documentId);
  if (!state || state.running) return;
  state.running = true;
  while (state.next) {
    const push = state.next;
    state.next = undefined;
    let note: string | null = null;
    try {
      note = await push.run();
    } catch {
      // A display failure must never reach the session.
    }
    // Refused or unanswered: the window doesn't hold this, so it can be sent again.
    if (!state.next && links.get(documentId)?.pushed !== state.lastJson) state.lastJson = undefined;
    push.done(note);
  }
  state.running = false;
}

/**
 * After a write: show the session in the app, without making the agent wait
 * for it. Latest wins: pushes made while one is in flight collapse to the
 * newest. Returns a note for the agent when the app answers quickly: once when
 * it goes live, and on every call the user's work keeps it off screen. A
 * slower answer's note comes back through `takeLinkNote`.
 */
export async function pushAfterWrite(
  documentId: string,
  tool: string,
  document: Document | undefined,
  call: Call = callApp,
  intent: LinkIntent = "implicit",
): Promise<string | null> {
  if (!document) return null;
  const json = JSON.stringify(document);
  const link = links.get(documentId);
  const state = pushes.get(documentId) ?? { running: false };
  pushes.set(documentId, state);
  const latest = state.lastJson ?? link?.pushed;
  if (link?.viewOnly || (link?.shown && latest === json && intent !== "explicit")) return null;
  state.lastJson = json;

  let returned = false;
  const finished = new Promise<string | null>((done) => {
    const superseded = state.next;
    state.next = {
      json,
      run: () => deliverPush(documentId, tool, document, json, call, intent),
      done: (note) => {
        if (note && returned) lateNotes.set(documentId, note);
        done(note);
      },
    };
    superseded?.done(null);
  });
  void drainPushes(documentId);
  // An explicit show_in_app is asking for the answer; everything else is a side effect.
  if (intent === "explicit") return finished;
  return Promise.race([
    finished,
    wait(PUSH_WAIT_MS).then(() => {
      returned = true;
      return null;
    }),
  ]);
}

async function deliverPush(
  documentId: string,
  tool: string,
  document: Document,
  json: string,
  call: Call,
  intent: LinkIntent,
): Promise<string | null> {
  const link = links.get(documentId);
  const opened = intent === "opened" || link?.opened === true;
  const effective: LinkIntent = intent === "implicit" && opened ? "opened" : intent;
  const base_dir = documentFolder(documentId, document);
  const reply = await call("adopt_document", {
    document,
    link_id: documentId,
    tool,
    intent: effective,
    ...(base_dir ? { base_dir } : {}),
  });
  if (reply?.adopted) {
    const first = !link?.shown;
    links.set(documentId, { pushed: json, shown: true, ...(opened ? { opened } : {}) });
    if (typeof reply.displaced === "string" && reply.displaced !== documentId) {
      loseWindow(reply.displaced, LOST_WINDOW);
    }
    return first ? "This document is live in the vcad app: the user sees each edit as one undo step." : null;
  }
  if (opened && !link) links.set(documentId, { pushed: "", shown: false, opened });
  if (reply?.reason === "linked") {
    return (
      "Not shown in the vcad app: its window is showing another session's build, and a session a tool " +
      "minted on its own never takes it. This edit exists only in this session. To show it, call show_in_app."
    );
  }
  if (reply?.reason === "linked_active") {
    return (
      "Not shown in the vcad app yet: the session on screen was active moments ago. " +
      "Your next edit takes the window once it has been idle for 30 seconds; show_in_app takes it now."
    );
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
  {
    name: "show_in_app",
    pack: null,
    description:
      "Put this session's document on screen in the vcad desktop app, taking the window from another " +
      "session if one holds it. Sessions never take a window on their own while another is working in it; " +
      "this is the deliberate way. Never replaces the user's own unsaved work. Returns whether it is shown.",
    inputSchema: {
      type: "object" as const,
      properties: { document_id: { type: "string", description: "Session to show." } },
      required: ["document_id"],
    },
    handler: async (args) => {
      const id = String((args as { document_id?: unknown }).document_id ?? "");
      const result = await showInApp(id, documents.get(id));
      return { content: [{ type: "text" as const, text: result.text }], ...(result.ok ? {} : { isError: true }) };
    },
    behavior: behavior({}),
  },
];

/** Deliberately put a session on screen; the only call that displaces a busy session. */
export async function showInApp(
  documentId: string,
  document: Document | undefined,
  call: Call = callApp,
): Promise<{ ok: boolean; text: string }> {
  if (!document) return { ok: false, text: `Unknown document_id: ${documentId}` };
  if (links.get(documentId)?.viewOnly) {
    return { ok: false, text: "This session is a view-only copy: only poses mirror in the app." };
  }
  const note = await pushAfterWrite(documentId, "show_in_app", document, call, "explicit");
  if (links.get(documentId)?.shown) return { ok: true, text: note ?? "This document is on screen in the vcad app." };
  return { ok: false, text: note ?? "No running vcad app was found. Open the vcad desktop app, then try again." };
}

const POSE_INTERVAL_MS = 33;

type PoseFrame = { args: Record<string, unknown>; call: Call };
let poseBusy = false;
let lastPose = 0;
/** A new episode's anchor: delivered before any later frame, never replaced by one. */
let pendingRestart: PoseFrame | null = null;
/** The newest frame not yet sent. Whatever is here when the stream stops still lands. */
let pendingLatest: PoseFrame | null = null;
let poseTimer: ReturnType<typeof setTimeout> | null = null;

function sendPose(frame: PoseFrame, now: number): void {
  poseBusy = true;
  lastPose = now;
  const done = () => {
    poseBusy = false;
    schedulePendingPose();
  };
  void frame.call("set_pose", frame.args).then(done, done);
}

function schedulePendingPose(): void {
  if (poseBusy || poseTimer || (!pendingRestart && !pendingLatest)) return;
  const wait = pendingRestart ? 0 : Math.max(0, lastPose + POSE_INTERVAL_MS - Date.now());
  poseTimer = setTimeout(() => {
    poseTimer = null;
    if (poseBusy) return;
    const frame = pendingRestart ?? pendingLatest;
    if (!frame) return;
    if (pendingRestart) pendingRestart = null;
    else pendingLatest = null;
    sendPose(frame, Date.now());
  }, wait);
}

/**
 * Mirror a simulation step in the app. Latest wins: a step that arrives while a
 * pose is in flight, or inside the frame interval, replaces the waiting frame so
 * a training loop is never slowed by the display, and the waiting frame is sent
 * when the slot frees, so the last pose of a run always lands. Returns whether
 * this frame went out now. Multi-DOF joints have no angle to set.
 */
export function pushPose(
  documentId: string,
  joints: { id: string; position: number; positions?: number[] }[],
  call: Call = callApp,
  now: number = Date.now(),
  base?: { pose: number[]; restart: boolean },
): boolean {
  const link = links.get(documentId);
  if (!link?.shown) return false;
  const pose = Object.fromEntries(joints.filter((j) => !j.positions).map((j) => [j.id, j.position]));
  if (Object.keys(pose).length === 0 && !base) return false;
  // `partial`: a stream carries joints the window doesn't drive; pose what it can.
  const args: Record<string, unknown> = {
    joints: pose,
    partial: true,
    ...(link.viewOnly ? {} : { link_id: documentId }),
    ...(base ? { base: base.pose, restart: base.restart } : {}),
  };
  const frame: PoseFrame = { args, call };
  if (base?.restart) {
    pendingLatest = null;
    if (poseBusy) {
      pendingRestart = frame;
      return false;
    }
    pendingRestart = null;
    sendPose(frame, now);
    return true;
  }
  if (poseBusy || pendingRestart || now - lastPose < POSE_INTERVAL_MS) {
    pendingLatest = frame;
    schedulePendingPose();
    return false;
  }
  pendingLatest = null;
  sendPose(frame, now);
  return true;
}

/** Test seam: forget every link. */
export function resetAppLinks(): void {
  links.clear();
  pushes.clear();
  lateNotes.clear();
  poseBusy = false;
  pinned = null;
  warned = "";
  lastPose = 0;
  pendingRestart = null;
  pendingLatest = null;
  if (poseTimer) clearTimeout(poseTimer);
  poseTimer = null;
  folders.clear();
}
