/**
 * The first engine mutation after `create_cad_loon` must not re-solve parts
 * it didn't touch. The engine re-serializes ops with a different key order
 * than loon, and the per-root evaluation cache keyed on raw JSON, so the
 * first `delete` re-evaluated every surviving part — 7–9 s once a stator
 * built from booleans was in the document. Bounded by evaluation count, not
 * wall clock.
 */
import { describe, it, expect, beforeAll, beforeEach } from "vitest";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { Engine } from "@vcad/engine";
import { documents, getSession } from "../tools/session.js";
import { incrementalEvaluationCount, stableKey } from "../tools/document-solids.js";
import { createServer } from "../server.js";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.init();
});
beforeEach(() => {
  documents.clear();
});

type Out = { content: Array<{ type: string; text?: string }>; isError?: boolean };
const out = (r: Out) => JSON.parse(r.content.find((c) => c.type === "text")!.text!);

async function connect() {
  const server = await createServer(engine, { user: null });
  const client = new Client({ name: "t", version: "1" });
  const [a, b] = InMemoryTransport.createLinkedPair();
  await Promise.all([server.connect(b), client.connect(a)]);
  return async (name: string, args: Record<string, unknown>) =>
    (await client.callTool({ name, arguments: args })) as Out;
}

// A small stator (ring yoke ∪ 12 teeth) plus three plain parts. Low tooth
// count keeps the test quick; the cache behaviour doesn't depend on size.
const SRC = `[root-named [translate 140 30 0 [union [difference [cylinder 30 20] [cylinder 40 20]]
  [circular-pattern 0 0 0 0 0 1 12 360 [translate 18 -3 0 [cube 14 6 20]]]]] "steel" "stator"]
[root-named [cube 10 10 10] "steel" "a"]
[root-named [translate 30 0 0 [cube 10 10 10]] "steel" "b"]
[root-named [translate 60 0 0 [cylinder 5 10]] "steel" "c"]`;

describe("stableKey", () => {
  it("ignores object key order", () => {
    expect(stableKey({ a: 1, b: { c: [1, { d: 2, e: 3 }] } })).toBe(
      stableKey({ b: { c: [1, { e: 3, d: 2 }] }, a: 1 }),
    );
    expect(stableKey({ a: 1 })).not.toBe(stableKey({ a: 2 }));
  });
});

describe("first delete after loon authoring", () => {
  it("re-evaluates nothing, removes only the target, and undoes", async () => {
    const call = await connect();
    const id: string = out(await call("open_document", {})).document_id;
    expect((await call("create_cad_loon", { document_id: id, source: SRC })).isError).toBeFalsy();
    const rootsBefore = getSession(id).roots.length;

    const before = incrementalEvaluationCount();
    const del = await call("delete", { document_id: id, part_id: "a" });
    expect(del.isError).toBeFalsy();
    // Every surviving part is unchanged geometry: zero fresh evaluations.
    expect(incrementalEvaluationCount() - before).toBe(0);

    const body = out(del);
    expect(body.removed_features).toEqual([expect.objectContaining({ name: "a", kind: "cube" })]);
    expect(body.integrity).toBeDefined();
    expect(getSession(id).roots.length).toBe(rootsBefore - 1);

    const second = incrementalEvaluationCount();
    expect((await call("delete", { document_id: id, part_id: "b" })).isError).toBeFalsy();
    expect(incrementalEvaluationCount() - second).toBe(0);

    expect((await call("undo", { document_id: id })).isError).toBeFalsy();
    expect(getSession(id).roots.length).toBe(rootsBefore - 1);
  }, 120_000);
});
