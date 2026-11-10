import { describe, it, expect, afterEach } from "vitest";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Document } from "@vcad/ir";
import {
  InMemorySessionStore,
  FileSessionStore,
  createSessionStore,
  isSessionStoreDurable,
  sessionStoreInfo,
} from "../session-store.js";
import {
  documents,
  hydrateSession,
  getSession,
  getDocumentTool,
} from "../tools/session.js";

/** Minimal one-cube Document, same shape used across tools.test.ts. */
function makeCubeDoc(): Document {
  return {
    version: "0.1",
    nodes: {
      "1": {
        id: 1,
        name: "test_cube",
        op: { type: "Cube", size: { x: 10, y: 10, z: 10 } },
      },
    },
    materials: {},
    part_materials: {},
    roots: [{ root: 1, material: "default" }],
  } as unknown as Document;
}

const dirs: string[] = [];
function tempDir(): string {
  const d = mkdtempSync(join(tmpdir(), "vcad-session-store-"));
  dirs.push(d);
  return d;
}

afterEach(() => {
  documents.clear();
  for (const d of dirs.splice(0)) rmSync(d, { recursive: true, force: true });
  delete process.env.VCAD_MCP_DISK_SESSIONS;
});

describe("restart recovery (hydrate on cache miss)", () => {
  it("rehydrates a cleared cache from the disk store", async () => {
    const store = new FileSessionStore(tempDir());
    await store.save("doc_cold", makeCubeDoc());
    documents.clear();
    expect(documents.has("doc_cold")).toBe(false);

    expect(await hydrateSession(store, "doc_cold")).toBe(true);
    const fetched = JSON.parse(
      getDocumentTool({ document_id: "doc_cold" }).content[0].text,
    ) as Document;
    expect(fetched.roots).toHaveLength(1);
    expect(Object.keys(fetched.nodes)).toContain("1");
  });
});

describe("in-memory store (disk sessions opted out)", () => {
  it("load always misses and getSession still throws the pinned error", async () => {
    const store = new InMemorySessionStore();
    expect(await store.load()).toBeNull();
    expect(await hydrateSession(store, "doc_missing")).toBe(false);
    expect(() => getSession("doc_missing")).toThrow(/Unknown document_id/);
  });
});

describe("createSessionStore factory (local only)", () => {
  it("returns the file store by default, and reports durable", () => {
    expect(createSessionStore(null)).toBeInstanceOf(FileSessionStore);
    expect(isSessionStoreDurable()).toBe(true);
    expect(sessionStoreInfo().session_store).toBe("file");
  });

  it("ignores Supabase env — there is no hosted backend", () => {
    process.env.SUPABASE_URL = "https://supa.test";
    process.env.SUPABASE_SERVICE_ROLE_KEY = "k";
    try {
      expect(
        createSessionStore({ sub: "user-me", email: "a@b.c" }),
      ).toBeInstanceOf(FileSessionStore);
    } finally {
      delete process.env.SUPABASE_URL;
      delete process.env.SUPABASE_SERVICE_ROLE_KEY;
    }
  });

  it("falls back to in-memory when disk sessions are opted out", () => {
    process.env.VCAD_MCP_DISK_SESSIONS = "0";
    expect(createSessionStore(null)).toBeInstanceOf(InMemorySessionStore);
    expect(isSessionStoreDurable()).toBe(false);
    expect(sessionStoreInfo()).toEqual({ durable: false, session_store: "in-memory" });
  });
});
