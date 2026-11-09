/**
 * Regressions from the third pass of the end-to-end actuator acceptance run:
 * cavity detection, adding a board to a mechanical document, loon part edits,
 * topology-optimization contact, real board extent for clearance, and
 * resolution choice for thin sections.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { Engine } from "@vcad/engine";
import type { Document } from "@vcad/ir";
import { clearanceReceiptClaims, computeGroupClearance } from "../tools/clearance.js";
import { boardFromSolid, createSchematic, placeComponents } from "../tools/ecad.js";
import { checkEnclosureFit } from "../tools/enclosure.js";
import { getNodePcb, getPcbNodeIds } from "@vcad/core";
import { importKicad } from "../tools/import-pcb.js";
import { toolDefs as loonToolDefs } from "../tools/loon.js";
import { runMutation } from "../tools/registry-dispatch.js";
import { partIdForRoot } from "../tools/part-ids.js";
import { getSession, openDocument } from "../tools/session.js";
import { toolDefs as structureToolDefs } from "../tools/structure.js";
import { poseableParts } from "../tools/sweep.js";
import type { ToolContext } from "../tools/tool-def.js";
import { loadPathGap } from "../tools/topopt.js";

let engine: Engine;
let ctx: ToolContext;
const createCadLoon = loonToolDefs.find((t) => t.name === "create_cad_loon")!;
const analyzeStructure = structureToolDefs.find((t) => t.name === "analyze_structure")!;

beforeAll(async () => {
  engine = await Engine.init();
  ctx = { engine, user: null } as unknown as ToolContext;
});

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const out = (r: { content: Array<{ type?: string; text?: string }> }): any =>
  JSON.parse(r.content.find((c) => c.text !== undefined)!.text!);

const pins = [
  { number: "1", name: "A", type: "Passive", x: -5, y: 0 },
  { number: "2", name: "B", type: "Passive", x: 5, y: 0 },
];

async function board(footprint = "Resistor_SMD:R_0805", documentId?: string): Promise<string> {
  const { document_id: id } = out(
    await createSchematic({
      ...(documentId ? { document_id: documentId } : {}),
      components: [{ ref: "U1", value: "x", footprint, x: 0, y: 0, pins }],
    }),
  );
  out(await placeComponents({ document_id: id, board_width: 30, board_height: 30 }));
  return id as string;
}

function ir(nodes: Array<Record<string, unknown>>, root: number): Document {
  return {
    version: "0.1",
    nodes: Object.fromEntries(nodes.map((n) => [String(n.id), n])),
    materials: {},
    part_materials: {},
    roots: [{ root, material: "default" }],
  } as unknown as Document;
}

let nextId = 1;
const boxAt = (nodes: Array<Record<string, unknown>>, size: number[], at: number[]): number => {
  const cube = nextId++;
  const placed = nextId++;
  nodes.push({ id: cube, name: null, op: { type: "Cube", size: { x: size[0], y: size[1], z: size[2] } } });
  nodes.push({ id: placed, name: null, op: { type: "Translate", child: cube, offset: { x: at[0], y: at[1], z: at[2] } } });
  return placed;
};

/** 60×40×20 case, 56×36×16 interior, minus each listed opening box. */
function closedCase(openings: Array<[number[], number[]]>): Document {
  nextId = 1;
  const nodes: Array<Record<string, unknown>> = [];
  let body = boxAt(nodes, [60, 40, 20], [0, 0, 0]);
  const cut = (tool: number) => {
    const id = nextId++;
    nodes.push({ id, name: null, op: { type: "Difference", left: body, right: tool } });
    body = id;
  };
  cut(boxAt(nodes, [56, 36, 16], [2, 2, 2]));
  for (const [size, at] of openings) cut(boxAt(nodes, size, at));
  return ir(nodes, body);
}

// A USB opening through the -X wall and a 9×14 slot through the lid.
const CONNECTOR: [number[], number[]] = [[6, 9, 5], [-2, 15, 8]];
const LID_SLOT: [number[], number[]] = [[14, 9, 6], [20, 15, 16]];

describe("finding the interior cavity", () => {
  it("finds it through connector cutouts and a lid slot", async () => {
    const enclosure = out(openDocument({ initial: closedCase([CONNECTOR, LID_SLOT]) })).document_id;
    const res = out(await checkEnclosureFit({ document_id: await board(), enclosure_document_id: enclosure }, engine));
    expect(res.success).toBe(true);
    expect(res.cavity_basis).toBe("sealed-void");
    expect(res.cavity.floorZ).toBeCloseTo(2, 1);
    expect(res.cavity.ceilZ).toBeCloseTo(18, 1);
    expect(res.cavity.maxX - res.cavity.minX).toBeGreaterThan(50);
    expect(res.cavity.maxY - res.cavity.minY).toBeGreaterThan(30);
  });

  it("traces the cavity, not the slot, for board_from_solid", () => {
    const id = out(openDocument({ initial: closedCase([CONNECTOR, LID_SLOT]) })).document_id;
    const res = out(boardFromSolid({ document_id: id, region: "cavity" }, engine));
    const xs = res.outline.vertices.map((v: { x: number }) => v.x);
    const ys = res.outline.vertices.map((v: { y: number }) => v.y);
    expect(Math.max(...xs) - Math.min(...xs)).toBeGreaterThan(50);
    expect(Math.max(...ys) - Math.min(...ys)).toBeGreaterThan(30);
  });

  it("still refuses a solid block, with or without a slot through it", async () => {
    nextId = 1;
    const nodes: Array<Record<string, unknown>> = [];
    const a = boxAt(nodes, [60, 40, 20], [0, 0, 0]);
    const b = boxAt(nodes, [14, 9, 40], [20, 15, -10]);
    nodes.push({ id: nextId, name: null, op: { type: "Difference", left: a, right: b } });
    const slotted = out(openDocument({ initial: ir(nodes, nextId) })).document_id;
    const res = await checkEnclosureFit({ document_id: await board(), enclosure_document_id: slotted }, engine);
    expect(res.isError).toBe(true);
    expect(res.content[0]!.text).toMatch(/no interior cavity/i);
  });
});

describe("adding a board to a mechanical document", () => {
  it("creates the schematic and board inside the existing document", async () => {
    const { document_id: id } = out(openDocument({ initial: closedCase([]) }));
    const before = getSession(id).roots.length;
    await board(undefined, id);
    const doc = getSession(id);
    expect(doc.roots.length).toBe(before + 1);
    expect(getPcbNodeIds(doc).length).toBe(1);
    expect(getNodePcb(doc, getPcbNodeIds(doc)[0]!)!.footprints.length).toBe(1);
  });

  it("refuses a second one and changes nothing", async () => {
    const id = await board();
    const snapshot = JSON.stringify(getSession(id));
    const res = await createSchematic({
      document_id: id,
      components: [{ ref: "U2", value: "x", footprint: "Resistor_SMD:R_0805", x: 0, y: 0, pins }],
    });
    expect(res.isError).toBe(true);
    expect(JSON.stringify(getSession(id))).toBe(snapshot);
  });

  it("imports a board into an existing document, all or nothing", async () => {
    const KICAD = `(kicad_pcb (version 20221018) (generator test)
      (general (thickness 1.6))
      (layers (0 "F.Cu" signal) (31 "B.Cu" signal))
      (net 0 "")
      (gr_line (start 0 0) (end 30 0) (layer "Edge.Cuts") (width 0.1))
      (gr_line (start 30 0) (end 30 20) (layer "Edge.Cuts") (width 0.1))
      (gr_line (start 30 20) (end 0 20) (layer "Edge.Cuts") (width 0.1))
      (gr_line (start 0 20) (end 0 0) (layer "Edge.Cuts") (width 0.1)))`;
    const content_base64 = Buffer.from(KICAD).toString("base64");
    const { document_id: id } = out(openDocument({ initial: closedCase([]) }));
    const first = out(await importKicad({ content_base64, document_id: id }));
    expect(first.document_id).toBe(id);
    const doc = getSession(id);
    expect(getPcbNodeIds(doc).length).toBe(1);
    expect(doc.roots.length).toBe(2);
    const snapshot = JSON.stringify(doc);
    const second = await importKicad({ content_base64, document_id: id });
    expect(second.isError).toBe(true);
    expect(JSON.stringify(getSession(id))).toBe(snapshot);
  });
});

describe("editing loon-authored parts", () => {
  const LOON = "[difference [cube 10 10 10] [cube 2 2 40]]";
  const bbox = (doc: Document) => {
    const p = poseableParts(doc, engine)[0]!.localMesh.positions;
    let [lo, hi] = [Infinity, -Infinity];
    for (let i = 0; i < p.length; i += 3) {
      lo = Math.min(lo, p[i]!);
      hi = Math.max(hi, p[i]!);
    }
    return [lo, hi];
  };

  it("moves a boolean part authored at the origin", async () => {
    const { document_id: id } = out(openDocument({}));
    await createCadLoon.handler({ document_id: id, source: LOON }, ctx);
    const doc = getSession(id);
    const [lo0] = bbox(doc);
    runMutation("update", { node_id: String(doc.roots[0]!.root), params: { offset: { x: 25 } } }, doc, id);
    expect(bbox(doc)[0]).toBeCloseTo(lo0! + 25, 3);
  });

  it("removes what only a deleted part used, and reports it", async () => {
    const { document_id: id } = out(openDocument({}));
    await createCadLoon.handler({ document_id: id, source: LOON }, ctx);
    const doc = getSession(id);
    expect(doc.roots.length).toBe(1);
    const part = partIdForRoot(doc, doc.roots[0]!);
    const res = out(runMutation("delete", { part_id: part }, doc, id));
    expect(doc.roots.length).toBe(0);
    expect(res.removed_features.length).toBe(3);
    expect(res.removed_features.map((f: { kind: string }) => f.kind).sort()).toEqual(["boolean", "cube", "cube"]);
  });
});

describe("topology_optimize contact check", () => {
  const box = (lo: number[], hi: number[], base = 0) => ({
    positions: [
      lo[0], lo[1], lo[2], hi[0], lo[1], lo[2], hi[0], hi[1], lo[2], lo[0], hi[1], lo[2],
      lo[0], lo[1], hi[2], hi[0], lo[1], hi[2], hi[0], hi[1], hi[2], lo[0], hi[1], hi[2],
    ] as number[],
    indices: [0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7, 6, 3, 0, 4, 3, 4, 7].map(
      (i) => i + base,
    ),
  });
  const merge = (a: ReturnType<typeof box>, b: ReturnType<typeof box>) => ({
    positions: [...a.positions, ...b.positions],
    indices: [...a.indices, ...b.indices],
  });
  const support = { region: { min: [0, 0, 0], max: [0, 10, 10] } };
  const load = (x: number) => ({ region: { min: [x, 0, 0], max: [x, 10, 10] } });

  it("refuses a 3 mm gap between material and the load patch", () => {
    // The reported run: voxel-size slack of 2 made x=109 "touch" x=112.
    const mesh = box([0, 0, 0], [109, 10, 10]);
    expect(loadPathGap(mesh, { loads: [load(112)], supports: [support] }, 1.5)).toMatch(/no material at its region/);
  });

  it("accepts material within a fraction of a voxel of the patch", () => {
    const mesh = box([0, 0, 0], [111.5, 10, 10]);
    expect(loadPathGap(mesh, { loads: [load(112)], supports: [support] }, 1.5)).toBeNull();
  });

  it("refuses a tine not joined back to the support", () => {
    const mesh = merge(box([0, 0, 0], [100, 4, 10]), box([60, 6, 0], [100, 10, 10], 8));
    expect(loadPathGap(mesh, { loads: [load(100)], supports: [support] }, 1)).toMatch(/not connected to any support/);
  });

  it("refuses loads carried by separate pieces", () => {
    const mesh = merge(box([0, 0, 0], [100, 4, 10]), box([0, 6, 0], [50, 10, 10], 8));
    const spec = {
      loads: [{ region: { min: [100, 0, 0], max: [100, 4, 10] } }, { region: { min: [50, 6, 0], max: [50, 10, 10] } }],
      supports: [support],
    };
    expect(loadPathGap(mesh, spec, 1)).toMatch(/separate pieces/);
  });
});

describe("clearance against a board", () => {
  const TALL = "Package_SO:SOIC-8_3.9x4.9mm_P1.27mm";

  const gapToBlock = (doc: Document, boardId: string, blockId: string) =>
    computeGroupClearance(doc, engine, [boardId], [blockId]).result!;

  it("measures to the component bodies, and keeps doing so after a delete", async () => {
    const id = await board(TALL);
    const doc = getSession(id);
    const bodies = poseableParts(doc, engine)[0]!;
    let top = -Infinity;
    for (let i = 2; i < bodies.localMesh.positions.length; i += 3) top = Math.max(top, bodies.localMesh.positions[i]!);
    expect(top).toBeGreaterThan(2.2); // the SOIC body, not the 1 mm kernel box

    const boardId = bodies.id;
    const block = out(runMutation("create", { type: "cube", params: { size: { x: 40, y: 40, z: 5 } }, name: "lid" }, doc, id));
    out(runMutation("update", { node_id: block.part_id, params: { offset: { x: 0, y: 0, z: top - 0.3 } } }, doc, id));
    const spare = out(runMutation("create", { type: "cube", params: { size: { x: 1, y: 1, z: 1 } }, name: "spare" }, doc, id));

    const before = gapToBlock(doc, boardId, block.part_id);
    expect(before.intersecting || before.distance_mm <= 0).toBe(true);
    runMutation("delete", { part_id: spare.part_id }, doc, id);
    const after = gapToBlock(doc, boardId, block.part_id);
    expect(after.intersecting || after.distance_mm <= 0).toBe(true);
  });
});

describe("receipts over many clearance specs", () => {
  it("measures each part pair once across specs", async () => {
    const { document_id: id } = out(openDocument({ initial: closedCase([]) }));
    const doc = getSession(id);
    const other = out(runMutation("create", { type: "cube", params: { size: { x: 1, y: 1, z: 1 } }, name: "o" }, doc, id));
    const caseId = poseableParts(doc, engine)[0]!.id;
    doc.clearance_specs = [1, 2, 3].map((n) => ({
      label: `spec${n}`,
      group_a: [caseId],
      group_b: [other.part_id],
      min_mm: -1000,
    })) as typeof doc.clearance_specs;
    const spy = vi.spyOn(engine, "meshClearance");
    const claims = clearanceReceiptClaims(doc, engine);
    expect(claims.length).toBe(3);
    expect(new Set(claims.map((c) => c.measured?.value)).size).toBe(1);
    expect(spy).toHaveBeenCalledTimes(1);
    spy.mockRestore();
  });
});

describe("analyze_structure on a thin part", () => {
  it("raises the default resolution to resolve the section", async () => {
    nextId = 1;
    const nodes: Array<Record<string, unknown>> = [];
    const root = boxAt(nodes, [100, 10, 5], [0, 0, 0]);
    const { document_id: id } = out(openDocument({ initial: ir(nodes, root) }));
    const res = out(
      (await analyzeStructure.handler(
        {
          document_id: id,
          part: partIdForRoot(getSession(id), getSession(id).roots[0]!),
          loads: [{ region: { min: [99, 0, 0], max: [100, 10, 5] }, force: [0, 0, -5] }],
          supports: [{ region: { min: [0, 0, 0], max: [1, 10, 5] } }],
        },
        ctx,
      )) as { content: Array<{ text: string }> },
    );
    expect(res.resolution_used).toBeGreaterThan(24);
    expect(res.study.thin_wall.cells_through_section).toBeGreaterThanOrEqual(5.5);
  });
});
