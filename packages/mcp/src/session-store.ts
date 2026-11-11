/**
 * Backing store for MCP document sessions — local only.
 *
 * vcad 1.0 runs the MCP server on the user's own machine. Sessions persist to
 * disk (`~/.vcad/mcp-sessions`, override with `VCAD_MCP_SESSION_DIR`, opt out
 * with `VCAD_MCP_DISK_SESSIONS=0`) so a restart doesn't drop an open build.
 * The hosted Supabase backends were removed with the hosted services.
 */
import fs from "node:fs/promises";
import nodePath from "node:path";
import os from "node:os";
import type { Document } from "@vcad/ir";
import type { AuthUser } from "./auth-user.js";

export interface SessionStore {
  /** Keying scope of the durable rows. "user": rows are scoped to the caller
   *  (user_id, local_id), so human-chosen names are safe keys. "capability":
   *  rows are keyed by the unguessable id ALONE, so name-derived keys would be
   *  guessable across tenants — only random ids are safe. "memory": no durable
   *  backend at all (stdio/local). save_document keys its named saves off this. */
  readonly scope: "memory" | "user" | "capability";
  /** Fetch a session's Document, or null on miss. Never throws for
   *  not-found; read errors are logged and surfaced as null so
   *  an outage degrades to "cache only", not a tool failure. */
  load(documentId: string): Promise<Document | null>;
  /** Create-or-update the durable row for this session. Idempotent on
   *  (user_id, local_id). Best-effort: errors are logged, never thrown. */
  save(documentId: string, doc: Document, name?: string): Promise<void>;
  /** Forget the durable row (close_document). Best-effort. */
  drop(documentId: string): Promise<void>;
}

/**
 * No-op store = today's behavior: the `documents` cache is the source of truth,
 * load always misses (so getSession throws "Unknown document_id" exactly as
 * before), save/drop do nothing. Used for stdio and anonymous HTTP calls.
 */
export class InMemorySessionStore implements SessionStore {
  readonly scope = "memory" as const;
  async load(): Promise<Document | null> {
    return null;
  }
  async save(): Promise<void> {
    /* the in-memory cache is the source of truth */
  }
  async drop(): Promise<void> {
    /* nothing durable to forget */
  }
}

/**
 * Disk-backed store for LOCAL runs (stdio / `npx @vcad/mcp` / a dev server on a
 * developer's machine). One JSON file per session under the session directory.
 *
 * WHY this is the default: an agent can spend 30+ turns authoring an
 * assembly, and a process restart under it used to vaporize the work with no
 * signal. A file per session is the cheapest thing that survives a restart,
 * needs no configuration, and keeps the load/save/drop contract — so the
 * hydrate-on-miss dispatch path rehydrates a session after a restart.
 *
 * Best-effort throughout: a read failure is a
 * miss (the warm cache still serves), a write failure is logged and never turns
 * a successful tool call into an error.
 */
export class FileSessionStore implements SessionStore {
  readonly scope = "capability" as const;
  constructor(private dir: string) {}

  /** Session ids are `doc_<n>_<base64url>`; anything else can't have been
   *  minted by us, and must never be able to escape the session directory. */
  private pathFor(documentId: string): string | null {
    if (!/^[A-Za-z0-9_-]{1,128}$/.test(documentId)) return null;
    return nodePath.join(this.dir, `${documentId}.json`);
  }

  async load(documentId: string): Promise<Document | null> {
    const file = this.pathFor(documentId);
    if (!file) return null;
    try {
      const raw = await fs.readFile(file, "utf8");
      return unwrapDocument(JSON.parse(raw) as unknown);
    } catch (err) {
      // ENOENT is the ordinary miss — only log something unexpected.
      if ((err as NodeJS.ErrnoException)?.code !== "ENOENT") {
        console.error("[session-store] file load failed:", err);
      }
      return null;
    }
  }

  async save(documentId: string, doc: Document): Promise<void> {
    const file = this.pathFor(documentId);
    if (!file) return;
    try {
      await fs.mkdir(this.dir, { recursive: true });
      // Write-then-rename: a crash mid-write leaves the previous good snapshot
      // rather than a truncated file that would fail to parse on rehydrate.
      const tmp = `${file}.${process.pid}.tmp`;
      await fs.writeFile(tmp, JSON.stringify(doc), "utf8");
      await fs.rename(tmp, file);
    } catch (err) {
      console.error("[session-store] file save failed:", err);
    }
  }

  async drop(documentId: string): Promise<void> {
    const file = this.pathFor(documentId);
    if (!file) return;
    try {
      await fs.rm(file, { force: true });
    } catch (err) {
      console.error("[session-store] file drop failed:", err);
    }
  }
}

/** Where local sessions persist: `VCAD_MCP_SESSION_DIR`, else
 *  `~/.vcad/mcp-sessions`. */
export function sessionDir(): string {
  return (
    process.env.VCAD_MCP_SESSION_DIR ||
    nodePath.join(os.homedir(), ".vcad", "mcp-sessions")
  );
}

/**
 * True when sessions should persist to disk: always, unless explicitly opted
 * out with `VCAD_MCP_DISK_SESSIONS=0`.
 */
export function useFileSessionStore(): boolean {
  if (/^(0|false|off)$/i.test(process.env.VCAD_MCP_DISK_SESSIONS || "")) {
    return false;
  }
  return true;
}

/** True when `content` looks like a raw Document IR (has nodes + roots). */
function looksLikeDocument(content: unknown): content is Document {
  return (
    !!content &&
    typeof content === "object" &&
    "nodes" in (content as object) &&
    "roots" in (content as object)
  );
}

/** Unwrap a stored blob into a Document IR; tolerate a legacy `{ ir }` envelope. */
function unwrapDocument(content: unknown): Document | null {
  if (looksLikeDocument(content)) return content;
  const env = content as { ir?: unknown } | null;
  if (env && looksLikeDocument(env.ir)) return env.ir;
  return null;
}

// ─── Durability self-report (server_info / /health) ───────────────────────────

/** True when sessions survive a server restart (the disk store is on). */
export function isSessionStoreDurable(): boolean {
  return useFileSessionStore();
}

/** Durability state for `server_info` and the `/health` endpoint. */
export function sessionStoreInfo(): {
  durable: boolean;
  session_store: "file" | "in-memory";
  /** Where file-backed sessions live — only set for the "file" store. */
  session_dir?: string;
} {
  const file = useFileSessionStore();
  return {
    durable: file,
    session_store: file ? "file" : "in-memory",
    ...(file ? { session_dir: sessionDir() } : {}),
  };
}

/** Choose the store impl: disk-backed by default, in-memory when opted out. */
export function createSessionStore(_user?: AuthUser | null): SessionStore {
  if (useFileSessionStore()) return new FileSessionStore(sessionDir());
  return new InMemorySessionStore();
}

// ─── Event log ───────────────────────────────────────────────────────────────
//
// The hosted build appended every mutation to a Supabase `session_events`
// table for the live review window. Locally there is no spine; the no-op store
// keeps the call sites unchanged.

/** An event to append. `idempotencyKey` is generated when omitted. */
export interface SessionEvent {
  /** Emitter: a user sub, the literal "agent", or "human". */
  author: string;
  /** kernel = folds into geometry; overlay = annotation; control = lifecycle. */
  kind: "kernel" | "overlay" | "control";
  /** Fine type: the tool name for kernel, "pin"/"flag" for overlay, etc. */
  type: string;
  /** kernel: {tool, args, changed?}; overlay: {anchor, text, …}. */
  payload: Record<string, unknown>;
  /** Per-session idempotency key; a random uuid is used when omitted. */
  idempotencyKey?: string;
}

/** A row read back from `session_events`. */
export interface StoredSessionEvent {
  id: number;
  seq: number;
  session_id: string;
  author: string;
  kind: string;
  type: string;
  payload: Record<string, unknown>;
  created_at: string;
}

export interface SessionEventStore {
  /** Append one event. Best-effort: errors are logged, never thrown. */
  append(sessionId: string, evt: SessionEvent): Promise<void>;
  /** Read a session's events in seq order (replay / live window). With
   *  `sinceSeq`, only events after it — the backend filters server-side so a
   *  late-join catch-up doesn't pull the whole log. */
  list(sessionId: string, sinceSeq?: number): Promise<StoredSessionEvent[]>;
}

/** No-op store = stdio/local: nothing durable, list is empty. */
export class NoopSessionEventStore implements SessionEventStore {
  async append(): Promise<void> {
    /* no spine without Supabase env */
  }
  async list(): Promise<StoredSessionEvent[]> {
    return [];
  }
}

/** The only event store in the local build: nothing durable, list is empty. */
export function createSessionEventStore(
  _user?: AuthUser | null,
): SessionEventStore {
  return new NoopSessionEventStore();
}

// ─── Tool-pack preference (runtime tool-pack switching, issue #432) ───────────

export interface PackStore {
  /** Always false in the local build — the preference lives in-process. */
  readonly durable: boolean;
  /** The caller's saved enabled-pack list, or null when none is stored. */
  load(): Promise<string[] | null>;
  /** Persist the caller's enabled-pack list (no-op without a user key). */
  save(packs: string[]): Promise<void>;
}

/** Process-global preference map for the in-memory store, keyed by user id. */
const inMemoryPackPrefs = new Map<string, string[]>();

/** Test hook: clear the process-global in-memory pack preferences. */
export function resetInMemoryPackStore(): void {
  inMemoryPackPrefs.clear();
}

/** In-memory pack store keyed by user id (a no-op when there's no user). */
export class InMemoryPackStore implements PackStore {
  readonly durable = false;
  constructor(private userId: string | null) {}
  async load(): Promise<string[] | null> {
    if (!this.userId) return null;
    const v = inMemoryPackPrefs.get(this.userId);
    return v ? [...v] : null;
  }
  async save(packs: string[]): Promise<void> {
    if (!this.userId) return;
    inMemoryPackPrefs.set(this.userId, [...packs]);
  }
}

/** The pack store: always in-memory in the local build. */
export function createPackStore(user?: AuthUser | null): PackStore {
  return new InMemoryPackStore(user?.sub ?? null);
}
