/**
 * Regressions from acceptance run 9: phantom KiCad net numbers failing a clean
 * board's receipt, build_receipt picking a different enclosure than
 * check_enclosure_fit, phantom board changes in update/undo reports, and
 * inside-out solids reading as valid positive volume in inspect_cad.
 */
import { describe, it, expect, beforeAll, beforeEach } from "vitest";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { Engine } from "@vcad/engine";
import type { Document } from "@vcad/ir";
import { documents, getSession } from "../tools/session.js";
import { createServer } from "../server.js";
import { documentSolids } from "../tools/document-solids.js";

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

const pads = (ref: string, x: number) => `(footprint "R_0805" (layer "F.Cu") (at ${x} 20)
    (fp_text reference "${ref}" (at 0 0) (layer "F.SilkS"))
    (pad "1" smd rect (at -1 0) (size 1 1.2) (layers "F.Cu") (net 1 "+12V"))
    (pad "2" smd rect (at 1 0) (size 1 1.2) (layers "F.Cu") (net 2 "GND")))`;
const seg = (x1: number, y1: number, x2: number, y2: number, net: number) =>
  `(segment (start ${x1} ${y1}) (end ${x2} ${y2}) (width 0.25) (layer "F.Cu") (net ${net}))`;

/**
 * A routed two-rail board in KiCad's numbered-net form: every rail is fully
 * connected, plus a GND pour on B.Cu (KiCad references zone nets by NUMBER).
 * The net numbers (1, 2) are file-local handles, not net names.
 */
const RAILS_KICAD = `(kicad_pcb (version 20221018) (generator test)
  (general (thickness 1.6))
  (layers (0 "F.Cu" signal) (31 "B.Cu" signal) (44 "Edge.Cuts" user))
  (net 0 "") (net 1 "+12V") (net 2 "GND")
  (gr_line (start 0 0) (end 60 0) (layer "Edge.Cuts") (width 0.05))
  (gr_line (start 60 0) (end 60 40) (layer "Edge.Cuts") (width 0.05))
  (gr_line (start 60 40) (end 0 40) (layer "Edge.Cuts") (width 0.05))
  (gr_line (start 0 40) (end 0 0) (layer "Edge.Cuts") (width 0.05))
  ${pads("R1", 20)} ${pads("R2", 40)}
  ${seg(19, 20, 19, 30, 1)} ${seg(19, 30, 39, 30, 1)} ${seg(39, 30, 39, 20, 1)}
  ${seg(21, 20, 21, 12, 2)} ${seg(21, 12, 41, 12, 2)} ${seg(41, 12, 41, 20, 2)}
  (via (at 30 12) (size 0.8) (drill 0.4) (layers "F.Cu" "B.Cu") (net 2))
  (zone (net 2) (net_name "GND") (layer "B.Cu") (hatch edge 0.5)
    (connect_pads (clearance 0.3)) (min_thickness 0.25)
    (polygon (pts (xy 25 6) (xy 35 6) (xy 35 16) (xy 25 16))))
)`;

async function importRails(call: Awaited<ReturnType<typeof connect>>, documentId?: string) {
  const content_base64 = Buffer.from(RAILS_KICAD, "utf8").toString("base64");
  const r = await call("import_kicad", { content_base64, ...(documentId ? { document_id: documentId } : {}) });
  expect(r.isError, r.content[0]?.text).toBeFalsy();
  return out(r).document_id as string;
}

/** An open-top tray: 80×50×20, 5 mm walls, a USB cutter through one wall. */
const CASE = `[root [pipe [cube 80 50 20]
  [difference [translate 30 -1 6 [cube 10 6 4]]]
  [difference [translate 5 5 2 [cube 70 40 20]]]] "steel"]`;

describe("build_receipt on a clean imported KiCad board", () => {
  it("has no phantom numbered nets and passes", async () => {
    const call = await connect();
    const id = await importRails(call);
    expect(out(await call("run_drc", { document_id: id })).errors).toBe(0);

    const built = out(await call("build_receipt", { document_id: id }));
    const nets = (built.power_integrity as Array<{ net: string; total_pads: number; continuous: boolean }>)
      .map((p) => p.net)
      .sort();
    expect(nets).toEqual(["+12V", "GND"]);
    for (const p of built.power_integrity) {
      expect(p.total_pads, p.net).toBeGreaterThan(0);
      expect(p.continuous, p.net).toBe(true);
    }
    expect(built.unified_summary.failed, JSON.stringify(built.unified.claims.filter((c: { verdict: string }) => c.verdict === "fail"))).toBe(0);
    expect(built.unified_summary.overall).not.toBe("fail");
  });
});

describe("build_receipt enclosure selection on a multi-part document", () => {
  it("finds the case the way check_enclosure_fit does, past a denser stator", async () => {
    const call = await connect();
    const id: string = out(await call("open_document", {})).document_id;
    expect((await call("create_cad_loon", { document_id: id, mode: "add", source: CASE })).isError).toBeFalsy();
    // A finely tessellated stator beside the case: more mesh than the case,
    // smaller bounding box, and no cavity.
    const doc = getSession(id);
    const next = Math.max(...Object.keys(doc.nodes).map(Number)) + 1;
    doc.nodes[String(next)] = { id: next, name: "stator", op: { type: "Sphere", radius: 8, segments: 256 } } as Document["nodes"][string];
    doc.nodes[String(next + 1)] = {
      id: next + 1,
      name: "stator-at",
      op: { type: "Translate", child: next, offset: { x: -30, y: 25, z: 8 } },
    } as Document["nodes"][string];
    doc.roots.push({ root: next + 1, material: "steel" });
    await importRails(call, id);

    // Precondition: the stator out-meshes the case, so "largest mesh" picks it.
    const solids = documentSolids(getSession(id), engine);
    const stator = solids.find((s) => s.name === "stator-at")!;
    const kase = solids.find((s) => s !== stator)!;
    expect(stator.mesh.positions.length).toBeGreaterThan(kase.mesh.positions.length);

    const casePart = solids.find((s) => s !== stator && s.name !== "stator")!.id;
    const fitOf = (r: Out) => (r as unknown as { structuredContent: { enclosure_fit: unknown } }).structuredContent.enclosure_fit;

    // check_enclosure_fit told which part is the case…
    const fit = await call("check_enclosure_fit", { document_id: id, enclosure_document_id: id, enclosure_part_id: casePart });
    expect(fit.isError, fit.content[0]?.text).toBeFalsy();
    const expected = JSON.stringify(fitOf(fit));

    // …and build_receipt given the same part reaches the same verdict.
    const named = await call("build_receipt", { document_id: id, enclosure_document_id: id, enclosure_part_id: casePart });
    expect(out(named).enclosure_fit_error).toBeUndefined();
    expect(JSON.stringify(fitOf(named))).toBe(expected);

    // Unnamed, both tools still pick the case over the denser stator.
    const autoFit = await call("check_enclosure_fit", { document_id: id, enclosure_document_id: id });
    expect(JSON.stringify(fitOf(autoFit))).toBe(expected);
    const auto = await call("build_receipt", { document_id: id, enclosure_document_id: id });
    expect(out(auto).enclosure_fit_error).toBeUndefined();
    expect(JSON.stringify(fitOf(auto))).toBe(expected);
  });
});

describe("update / undo change reports on a board + case document", () => {
  it("report only the cutter's part, never phantom board or zone changes", async () => {
    const call = await connect();
    const id: string = out(await call("open_document", {})).document_id;
    expect((await call("create_cad_loon", { document_id: id, mode: "add", source: CASE })).isError).toBeFalsy();
    const casePart = (out(await call("read", { document_id: id })).parts as Array<{ id: string }>)[0]!.id;
    await importRails(call, id);
    const zonesBefore = JSON.stringify(
      (Object.values(getSession(id).nodes).find((n) => n.op.type === "PcbBoard")!.op as unknown as { board: { zones: unknown[] } }).board.zones,
    );

    const cutter = Object.entries(getSession(id).nodes).find(([, n]) => {
      const op = n.op as { type: string; offset?: { x: number } };
      return op.type === "Translate" && op.offset?.x === 30;
    })!;
    const moved = await call("update", {
      document_id: id,
      node_id: Number(cutter[0]),
      params: { offset: { x: 58, y: -1, z: 6 } },
    });
    expect(moved.isError, moved.content[0]?.text).toBeFalsy();
    const changed = out(moved).changed as { added: unknown[]; removed: unknown[]; modified: Array<{ part_id: string; name?: string }> };
    // Only the case moved its cutter; the board beside it is untouched.
    expect(changed.modified.map((p) => p.part_id), JSON.stringify(changed)).toEqual([casePart]);
    expect(changed.added).toEqual([]);
    expect(changed.removed).toEqual([]);

    const undone = out(await call("undo", { document_id: id }));
    const zoneChanges = (undone.changed as Array<{ kind: string }>).filter((c) => c.kind !== "part");
    expect(zoneChanges).toEqual([]);
    expect(undone.changed).toContainEqual(expect.objectContaining({ kind: "part", action: "modified" }));
    const zonesAfter = JSON.stringify(
      (Object.values(getSession(id).nodes).find((n) => n.op.type === "PcbBoard")!.op as unknown as { board: { zones: unknown[] } }).board.zones,
    );
    expect(zonesAfter).toBe(zonesBefore);
  });
});

describe("inspect_cad on an inside-out solid", () => {
  it("flags reversed winding instead of reporting a silent positive volume", async () => {
    const call = await connect();
    // A 10 mm cube mesh, every triangle wound inward.
    const positions = [0, 0, 0, 10, 0, 0, 10, 10, 0, 0, 10, 0, 0, 0, 10, 10, 0, 10, 10, 10, 10, 0, 10, 10];
    const outward = [0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 2, 3, 7, 2, 7, 6, 1, 2, 6, 1, 6, 5, 3, 0, 4, 3, 4, 7];
    const inward: number[] = [];
    for (let i = 0; i < outward.length; i += 3) inward.push(outward[i]!, outward[i + 2]!, outward[i + 1]!);
    const mk = (indices: number[], name: string): Document =>
      ({
        version: "0.1",
        nodes: { "1": { id: 1, name, op: { type: "ImportedMesh", positions, indices } } },
        materials: {},
        part_materials: {},
        roots: [{ root: 1, material: "default" }],
      }) as unknown as Document;

    const good = out(await call("inspect_cad", { document: mk(outward, "good") }));
    expect(good.volume_mm3).toBeCloseTo(1000, 3);
    expect(good.inside_out_parts).toBeUndefined();

    const bad = out(await call("inspect_cad", { document: mk(inward, "flipped") }));
    expect(bad.inside_out_parts).toEqual([{ name: "flipped", signed_volume_mm3: -1000 }]);
    expect((bad.warnings as string[]).some((w) => w.includes("inside out"))).toBe(true);
  });
});
