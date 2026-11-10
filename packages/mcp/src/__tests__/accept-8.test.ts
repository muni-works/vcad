/**
 * Regressions from acceptance run 8: `update` on an inner node of a loon
 * part, readable default names from `assemble`, and gym position tracking.
 */
import { describe, it, expect, beforeAll, beforeEach } from "vitest";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { Engine } from "@vcad/engine";
import type { Document } from "@vcad/ir";
import { documents, getSession } from "../tools/session.js";
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

type Op = { type: string; child?: number; offset?: { x: number; y: number; z: number }; size?: { x: number; y: number; z: number } };
const opOf = (doc: Document, id: number | string) => (doc.nodes[String(id)] as { op: Op }).op;

/** The Translate wrapping the 10×6×4 USB cutter cube, and that cube. */
function usbCutter(doc: Document): { translate: string; cube: string } {
  for (const [id, n] of Object.entries(doc.nodes)) {
    const op = (n as { op: Op }).op;
    if (op.type !== "Translate" || op.child === undefined) continue;
    let c = op.child;
    // Engine-materialized features wrap the primitive in Scale/Rotate.
    for (let i = 0; i < 3; i++) {
      const inner = opOf(doc, c);
      if (inner.type === "Cube") break;
      if (inner.child === undefined) break;
      c = inner.child;
    }
    const cube = opOf(doc, c);
    if (cube.type === "Cube" && cube.size?.x === 10 && cube.size?.y === 6) return { translate: id, cube: String(c) };
  }
  throw new Error("no usb cutter");
}

const CASE = `[root [pipe [cube 80 50 20]
  [difference [translate 30 -1 6 [cube 10 6 4]]]
  [difference [translate 5 5 2 [cube 70 40 20]]]] "steel"]`;

describe("update on an inner node of a loon part", () => {
  it("moves only the cutter it was given, accepts the create size shape, and undo reports the change", async () => {
    const call = await connect();
    const id: string = out(await call("open_document", {})).document_id;
    expect((await call("create_cad_loon", { document_id: id, mode: "add", source: CASE })).isError).toBeFalsy();
    const vol = async () => out(await call("inspect_cad", { document_id: id })).volume_mm3 as number;
    const v0 = await vol();

    const moved = await call("update", {
      document_id: id,
      node_id: Number(usbCutter(getSession(id)).translate),
      params: { offset: { x: 58, y: -1, z: 6 } },
    });
    expect(moved.isError, moved.content[0]?.text).toBeFalsy();

    const doc = getSession(id);
    expect(doc.roots).toHaveLength(1);
    // The case's own placement is untouched…
    const rootOp = opOf(doc, doc.roots[0]!.root);
    if (rootOp.type === "Translate") expect(rootOp.offset).toEqual({ x: 0, y: 0, z: 0 });
    // …and the cutter is where it was sent.
    const cutter = usbCutter(doc);
    expect(opOf(doc, cutter.translate).offset).toEqual({ x: 58, y: -1, z: 6 });
    // Moved within the same wall: same material removed.
    expect(Math.abs((await vol()) - v0)).toBeLessThan(1);

    const resized = await call("update", {
      document_id: id,
      node_id: Number(cutter.cube),
      params: { size: { x: 10, y: 6, z: 8 } },
    });
    expect(resized.isError, resized.content[0]?.text).toBeFalsy();
    const v2 = await vol();
    // Twice as tall a slot through a 5 mm wall: 10·5·4 more mm³ removed.
    expect(v0 - v2).toBeGreaterThan(150);

    const undone = out(await call("undo", { document_id: id }));
    expect(undone.changed).toContainEqual(expect.objectContaining({ kind: "part", action: "modified" }));
    expect(Math.abs((await vol()) - v0)).toBeLessThan(1);
  });
});

describe("assemble without names", () => {
  it("names instances and joints after their parts, never after ids", async () => {
    const call = await connect();
    const id: string = out(await call("open_document", {})).document_id;
    await call("create_cad_loon", {
      document_id: id,
      mode: "add",
      source: `[root [cube 20 20 60] "steel"]\n[root [translate 0 0 -60 [cube 20 20 60]] "steel"]\n[root [translate 0 0 -120 [cube 20 20 60]] "steel"]`,
    });
    const parts = out(await call("describe_scene", { document_id: id })).parts as Array<{ id: string }>;
    const r = await call("assemble", {
      document_id: id,
      instances: parts.map((p, i) => ({ id: `i${i}`, part: p.id })),
      joints: [
        { child: "i1", parent: "i0", kind: { type: "Revolute", axis: { x: 1, y: 0, z: 0 } } },
        { child: "i2", parent: "i1", name: "knee", kind: { type: "Revolute", axis: { x: 1, y: 0, z: 0 } } },
      ],
      ground: "i0",
    });
    expect(r.isError, r.content[0]?.text).toBeFalsy();
    const doc = getSession(id);
    const names = [...(doc.instances ?? []), ...(doc.joints ?? [])].map((x) => x.name ?? "");
    for (const n of names) {
      expect(n).not.toMatch(/^(\d+:\d+|i\d|joint\d+)?$/);
    }
    const instName = (k: string) => doc.instances!.find((i) => i.id === k)!.name;
    expect(doc.joints![0]!.name).toBe(`${instName("i0")} → ${instName("i1")}`);
    expect(doc.joints![1]!.name).toBe("knee");
    expect(new Set(doc.instances!.map((i) => i.name)).size).toBe(3);
  });
});
