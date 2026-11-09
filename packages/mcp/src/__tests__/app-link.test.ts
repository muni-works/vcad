import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  attachAppDocument,
  linkIntent,
  noteDocumentFolder,
  pushAfterWrite,
  pushPose,
  reconcileBeforeWrite,
  resetAppLinks,
  showInApp,
  takeLinkNote,
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
    expect(sent[1]).toMatchObject({ joints: {}, base, restart: true, link_id: "a" });
  });

  describe("a pose stream's last frame", () => {
    beforeEach(() => void vi.useFakeTimers());
    afterEach(() => void vi.useRealTimers());

    const knee = (position: number) => [{ id: "knee", position }];
    const shown = () => pushAfterWrite("a", "create", doc(1), async () => ({ adopted: true }));

    it("lands when a fast run finishes inside the first in-flight call", async () => {
      await shown();
      const sent: unknown[] = [];
      let release = () => {};
      const call = (_: string, a: Record<string, unknown>) => {
        sent.push(a.joints);
        return new Promise<Record<string, unknown> | null>((r) => (release = () => r({})));
      };
      for (let i = 1; i <= 120; i++) pushPose("a", knee(i), call, Date.now());
      expect(sent).toEqual([{ knee: 1 }]);
      release();
      await vi.advanceTimersByTimeAsync(100);
      expect(sent).toEqual([{ knee: 1 }, { knee: 120 }]);
    });

    it("lands when the run stops inside the frame interval", async () => {
      await shown();
      const sent: unknown[] = [];
      const call = async (_: string, a: Record<string, unknown>) => (sent.push(a.joints), {});
      pushPose("a", knee(1), call, Date.now());
      await vi.advanceTimersByTimeAsync(0);
      pushPose("a", knee(2), call, Date.now());
      pushPose("a", knee(3), call, Date.now());
      await vi.advanceTimersByTimeAsync(100);
      expect(sent).toEqual([{ knee: 1 }, { knee: 3 }]);
    });

    it("delivers an episode restart that arrived mid-flight, before the latest frame", async () => {
      await shown();
      const sent: Record<string, unknown>[] = [];
      let release = () => {};
      const call = (_: string, a: Record<string, unknown>) => {
        sent.push(a);
        return new Promise<Record<string, unknown> | null>((r) => (release = () => r({})));
      };
      const pose = [0, 0, 1, 1, 0, 0, 0];
      pushPose("a", knee(1), call, Date.now());
      pushPose("a", knee(2), call, Date.now(), { pose, restart: true });
      pushPose("a", knee(3), call, Date.now());
      release();
      await vi.advanceTimersByTimeAsync(0);
      expect(sent[1]).toMatchObject({ joints: { knee: 2 }, restart: true });
      release();
      await vi.advanceTimersByTimeAsync(100);
      expect(sent[2]).toMatchObject({ joints: { knee: 3 } });
      expect(sent).toHaveLength(3);
    });

    it("does not stall the stream when the app call fails", async () => {
      await shown();
      const sent: unknown[] = [];
      const call = async (_: string, a: Record<string, unknown>) => {
        sent.push(a.joints);
        throw new Error("gone");
      };
      pushPose("a", knee(1), call, Date.now());
      pushPose("a", knee(2), call, Date.now());
      await vi.advanceTimersByTimeAsync(100);
      expect(sent).toEqual([{ knee: 1 }, { knee: 2 }]);
    });
  });

  describe("taking the window", () => {
    it("treats only a minting open as deliberate", () => {
      expect(linkIntent("create_cad_loon", true)).toBe("opened");
      expect(linkIntent("open_document", true)).toBe("opened");
      expect(linkIntent("create_schematic", true)).toBe("opened");
      expect(linkIntent("create_cad_loon", false)).toBe("implicit");
      expect(linkIntent("topology_optimize", true)).toBe("implicit");
    });

    const refuse = (reason: string) => async () => ({ adopted: false, reason });

    it("tells a throwaway session it stayed off screen, on every call", async () => {
      expect(await pushAfterWrite("t", "topology_optimize", doc(1), refuse("linked"))).toMatch(/show_in_app/);
      expect(await pushAfterWrite("t", "update", doc(2), refuse("linked"))).toMatch(/show_in_app/);
      expect(pushPose("t", [{ id: "k", position: 1 }], async () => ({}), 1000)).toBe(false);
    });

    it("keeps a deliberately opened session's intent for its later edits", async () => {
      const intents: unknown[] = [];
      const call = async (_: string, a: Record<string, unknown>) => (
        intents.push(a.intent), intents.length === 1 ? { adopted: false, reason: "linked_active" } : { adopted: true }
      );
      expect(await pushAfterWrite("n", "create_cad_loon", doc(1), call, "opened")).toMatch(/idle/);
      expect(await pushAfterWrite("n", "update", doc(2), call)).toMatch(/live/);
      expect(intents).toEqual(["opened", "opened"]);
    });

    it("tells the losing session once, in-process", async () => {
      await pushAfterWrite("old", "create", doc(1), async () => ({ adopted: true }));
      await pushAfterWrite("new", "create", doc(2), async () => ({ adopted: true, displaced: "old" }), "explicit");
      expect(takeLinkNote("old")).toMatch(/no longer on screen/);
      expect(takeLinkNote("old")).toBeNull();
      expect(pushPose("old", [{ id: "k", position: 1 }], async () => ({}), 1000)).toBe(false);
      expect(takeLinkNote("new")).toBeNull();
    });

    it("tells a session that lost the window to another server process", async () => {
      await pushAfterWrite("old", "create", doc(1), async () => ({ adopted: true }));
      expect(await reconcileBeforeWrite("old", new Map(), async () => ({ linked: false }))).toBeNull();
      expect(takeLinkNote("old")).toMatch(/no longer on screen/);
    });

    it("says nothing if the session is back on screen by the time it would be told", async () => {
      await pushAfterWrite("old", "create", doc(1), async () => ({ adopted: true }));
      await reconcileBeforeWrite("old", new Map(), async () => ({ linked: false }));
      await pushAfterWrite("old", "update", doc(2), async () => ({ adopted: true }));
      expect(takeLinkNote("old")).toBeNull();
    });

    it("scopes poses to the session so a loser can't move another build", async () => {
      await pushAfterWrite("a", "create", doc(1), async () => ({ adopted: true }));
      const seen: Record<string, unknown>[] = [];
      pushPose("a", [{ id: "k", position: 1 }], async (_, a) => (seen.push(a), {}), 1000);
      expect(seen[0]).toMatchObject({ link_id: "a", partial: true });
    });

    it("show_in_app pushes with explicit intent even when nothing changed", async () => {
      const sent: Record<string, unknown>[] = [];
      const call = async (_: string, a: Record<string, unknown>) => (sent.push(a), { adopted: true });
      await pushAfterWrite("a", "create", doc(1), call);
      expect((await showInApp("a", doc(1), call)).ok).toBe(true);
      expect(sent[1]).toMatchObject({ intent: "explicit" });
    });

    it("show_in_app reports a refusal as a failure", async () => {
      const r = await showInApp("a", doc(1), refuse("busy"));
      expect(r).toMatchObject({ ok: false, text: expect.stringMatching(/user's own work/) });
      expect((await showInApp("a", undefined)).ok).toBe(false);
      expect((await showInApp("a", doc(1), async () => null)).text).toMatch(/No running vcad app/);
    });
  });
});
