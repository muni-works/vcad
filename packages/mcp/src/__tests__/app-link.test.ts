import { beforeEach, describe, expect, it } from "vitest";
import { pushAfterWrite, pushPose, reconcileBeforeWrite, resetAppLinks } from "../app-link.js";
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

  it("notes a busy window once", async () => {
    const busy = async () => ({ adopted: false, reason: "busy" });
    expect(await pushAfterWrite("a", "create", doc(1), busy)).toMatch(/other work/);
    expect(await pushAfterWrite("a", "update", doc(2), busy)).toBeNull();
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
});
