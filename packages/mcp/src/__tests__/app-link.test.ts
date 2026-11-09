import { beforeEach, describe, expect, it } from "vitest";
import {
  attachAppDocument,
  noteDocumentFolder,
  pushAfterWrite,
  pushPose,
  reconcileBeforeWrite,
  resetAppLinks,
} from "../app-link.js";
import type { Document } from "@vcad/ir";

const doc = (v: number) => ({ version: String(v), nodes: {}, roots: [] }) as unknown as Document;

describe("app link", () => {
  beforeEach(resetAppLinks);

  it("pushes a change once and says so the first time", async () => {
    const sent: string[] = [];
    const call = async (tool: string) => (sent.push(tool), { adopted: true, revision: "r" });
    expect(await pushAfterWrite("a", "create", doc(1), call)).toMatch(/live/);
    expect(await pushAfterWrite("a", "update", doc(1), call)).toBeNull();
    expect(await pushAfterWrite("a", "update", doc(2), call)).toBeNull();
    expect(sent).toEqual(["adopt_document", "adopt_document"]);
  });

  it("does nothing without an app", async () => {
    const none = async () => null;
    expect(await pushAfterWrite("a", "create", doc(1), none)).toBeNull();
    expect(await reconcileBeforeWrite("a", new Map(), none)).toBeNull();
  });

  it("says so on every call a window with user work refuses", async () => {
    const busy = async () => ({ adopted: false, reason: "busy" });
    expect(await pushAfterWrite("a", "create", doc(1), busy)).toMatch(/user's own work/);
    expect(await pushAfterWrite("a", "update", doc(2), busy)).toMatch(/user's own work/);
  });

  it("passes the folder a document came from", async () => {
    const seen: Record<string, unknown>[] = [];
    const call = async (_: string, a: Record<string, unknown>) => (seen.push(a), { adopted: true });
    noteDocumentFolder("a", "/robots/k1");
    await pushAfterWrite("a", "create", doc(1), call);
    expect(seen[0]!.base_dir).toBe("/robots/k1");
    const sourced = { ...doc(2), source: { path: "/parts/arm.loon" } } as unknown as Document;
    await pushAfterWrite("b", "create", sourced, call);
    expect(seen[1]!.base_dir).toBe("/parts");
    await pushAfterWrite("c", "create", doc(3), call);
    expect(seen[2]).not.toHaveProperty("base_dir");
  });

  describe("attach", () => {
    const store = (m: Map<string, Document>) => (id: string, d: Document) => void m.set(id, d);

    it("pulls the open document under the id it linked, without echoing it back", async () => {
      const sent: [string, Record<string, unknown>][] = [];
      const theirs = doc(7);
      const call = async (tool: string, a: Record<string, unknown>) => (
        sent.push([tool, a]), { attached: true, document: theirs, name: "bracket", base_dir: "/d" }
      );
      const docs = new Map<string, Document>();
      const r = await attachAppDocument(call, () => "s1", store(docs));
      expect(r).toMatchObject({ document_id: "s1", name: "bracket", linked: true });
      expect(sent).toEqual([["attach_document", { link_id: "s1" }]]);
      expect(docs.get("s1")).toBe(theirs);
      expect(await pushAfterWrite("s1", "noop", theirs, call)).toBeNull();
      expect(sent).toHaveLength(1);
      await pushAfterWrite("s1", "create", doc(8), call);
      expect(sent[1]![1].base_dir).toBe("/d");
    });

    it("keeps a view-only document a copy that only mirrors poses", async () => {
      const sent: string[] = [];
      const call = async (tool: string) => (
        sent.push(tool), { attached: true, document: doc(1), view_only: true }
      );
      const r = await attachAppDocument(call, () => "v", store(new Map()));
      expect(r).toMatchObject({ linked: false });
      expect(await pushAfterWrite("v", "create", doc(2), call)).toBeNull();
      expect(await reconcileBeforeWrite("v", new Map(), call)).toBeNull();
      expect(sent).toEqual(["attach_document"]);
      expect(pushPose("v", [{ id: "knee", position: 1 }], call, 1000)).toBe(true);
    });

    it("explains no app and an empty window", async () => {
      const docs = new Map<string, Document>();
      expect(await attachAppDocument(async () => null, () => "x", store(docs))).toMatchObject({
        error: expect.stringMatching(/No running vcad app/),
      });
      expect(
        await attachAppDocument(async () => ({ attached: false, reason: "empty" }), () => "x", store(docs)),
      ).toMatchObject({ error: expect.stringMatching(/no document open/) });
      expect(docs.size).toBe(0);
    });
  });

  it("takes the user's version and stops a stale edit", async () => {
    await pushAfterWrite("a", "create", doc(1), async () => ({ adopted: true }));
    const theirs = doc(9);
    const docs = new Map<string, Document>();
    const call = async (_: string, args: Record<string, unknown>) =>
      args.pull ? { linked: true, document: theirs } : { linked: true, edited: true };
    expect(await reconcileBeforeWrite("a", docs, call)).toMatch(/changed this document/);
    expect(docs.get("a")).toBe(theirs);
  });

  it("lets a write through when nothing changed", async () => {
    await pushAfterWrite("a", "create", doc(1), async () => ({ adopted: true }));
    const call = async () => ({ linked: true, edited: false });
    expect(await reconcileBeforeWrite("a", new Map(), call)).toBeNull();
  });

  it("mirrors poses for a shown session, latest wins", async () => {
    const sent: unknown[] = [];
    const call = async (_: string, a: Record<string, unknown>) => (sent.push(a.joints), {});
    const j = [{ id: "knee", position: 30 }, { id: "base", position: 0, positions: [0, 0, 0] }];
    expect(pushPose("a", j, call, 1000)).toBe(false);
    await pushAfterWrite("a", "create", doc(1), async () => ({ adopted: true }));
    expect(pushPose("a", j, call, 1000)).toBe(true);
    expect(sent).toEqual([{ knee: 30 }]);
    await Promise.resolve();
    expect(pushPose("a", j, call, 1010)).toBe(false);
    expect(pushPose("a", j, call, 1040)).toBe(true);
  });

  it("sends the floating base and never drops an episode's first pose", async () => {
    const sent: Record<string, unknown>[] = [];
    const call = async (_: string, a: Record<string, unknown>) => (sent.push(a), {});
    await pushAfterWrite("a", "create", doc(1), async () => ({ adopted: true }));
    const base = [0, 0, 1, 1, 0, 0, 0];
    expect(pushPose("a", [], call, 1000, { pose: base, restart: false })).toBe(true);
    await Promise.resolve();
    expect(pushPose("a", [], call, 1001, { pose: base, restart: true })).toBe(true);
    expect(sent[1]).toEqual({ joints: {}, base, restart: true });
  });
});
