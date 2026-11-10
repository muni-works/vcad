/**
 * Regressions from the end-to-end actuator acceptance run: one agent builds
 * motor, board, enclosure, leg link, physics and fab in one document.
 */
import { beforeAll, describe, expect, it, vi } from "vitest";
import { Engine, getKernelWasmSync } from "@vcad/engine";
import { getPcbNodeIds } from "@vcad/core";
import type { Document } from "@vcad/ir";
import { assembleInto } from "../tools/assemble.js";
import { boardFromSolid, createSchematic, fabPrepNextStep, placeComponents } from "../tools/ecad.js";
import { clearIncrementalEvaluation, evaluateIncrementally } from "../tools/document-solids.js";
import { addComponentOverhangCheck } from "../tools/enclosure.js";
import { computeIntegrity } from "../tools/integrity.js";
import { addLoonToSession, toolDefs as loonToolDefs } from "../tools/loon.js";
import { runMutation } from "../tools/registry-dispatch.js";
import { oversizedSvg } from "../tools/render.js";
import { loadResvgWasmForTest } from "../tools/resvg-loader.js";
import { getSession, registerSession } from "../tools/session-core.js";
import { readFileSync } from "node:fs";
import type { ToolContext } from "../tools/tool-def.js";
import { loadPathGap, topologyOptimizeTool } from "../tools/topopt.js";

let engine: Engine;
let ctx: ToolContext;
const createCadLoon = loonToolDefs.find((t) => t.name === "create_cad_loon")!;

beforeAll(async () => {
  engine = await Engine.init();
  ctx = { engine, user: null } as unknown as ToolContext;
});

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const out = (r: { content: Array<{ text: string }> }): any => JSON.parse(r.content[0]!.text);

async function boardSession(): Promise<string> {
  const pins = [
    { number: "1", name: "A", type: "Passive", x: -5, y: 0 },
    { number: "2", name: "B", type: "Passive", x: 5, y: 0 },
  ];
  const { document_id: id } = out(
    await createSchematic({
      components: [{ ref: "R1", value: "1k", footprint: "Resistor_SMD:R_0805", x: 0, y: 0, pins }],
    }),
  );
  out(await placeComponents({ document_id: id, board_width: 30, board_height: 30 }));
  return id as string;
}

const ASSEMBLY = `
[let tray [cube 60 40 20]]
[let link [cube 10 10 80]]
[assembly
  #[[part "tray" tray "aluminum"] [part "link" link "steel"]]
  #[[instance "tray1" "tray" 0 0 0] [instance "link1" "link" 100 0 0]]
  #[]
  "tray1"]`;

/** What a window's engine holds after loading `doc`: the app's adopt path. */
function throughEngine(doc: Document): Document {
  const wasm = getKernelWasmSync() as unknown as {
    WasmDocumentEngine: {
      withReplica(id: number): {
        import_ir(j: string): unknown;
        free(): void;
        apply_tool(n: string, a: string): string;
      };
    };
  };
  const e = wasm.WasmDocumentEngine.withReplica(0);
  try {
    e.import_ir(JSON.stringify(doc));
    const r = JSON.parse(
      e.apply_tool("create", JSON.stringify({ type: "cube", size: { x: 1, y: 1, z: 1 } })),
    );
    expect(r.ok, JSON.stringify(r.error)).toBe(true);
    return r.result.document as Document;
  } finally {
    e.free();
  }
}

/** A board the engine has materialized: its root is a Translate, not the board. */
async function materializedBoardSession(): Promise<string> {
  const id = await boardSession();
  runMutation("create", { type: "cube", params: { size: { x: 5, y: 5, z: 5 } } }, getSession(id), id);
  const doc = getSession(id);
  const board = getPcbNodeIds(doc)[0]!;
  expect(doc.roots.some((r) => r.root === board), "fixture: board must sit under a wrapper").toBe(false);
  return id;
}

const boardRooted = (doc: Document) => {
  const board = getPcbNodeIds(doc)[0]!;
  const reaches = (id: number): boolean => {
    const op = doc.nodes[String(id)]?.op as { child?: number } | undefined;
    return id === board || (op?.child !== undefined && reaches(op.child));
  };
  return doc.roots.some((r) => reaches(r.root));
};

const domain = {
  domain_box: { min: [0, 0, 0], max: [24, 6, 12] },
  loads: [{ region: { min: [24, 0, 0], max: [24, 6, 2] }, force: [0, 0, -100] }],
  supports: [{ region: { min: [0, 0, 0], max: [0, 6, 12] } }],
  resolution: 16,
  max_iterations: 8,
  volume_fraction: 0.35,
};

describe("a board stays in the document", () => {
  it("survives create_cad_loon with an assembly, in the session and through the engine", async () => {
    const id = await materializedBoardSession();
    await createCadLoon.handler({ document_id: id, source: ASSEMBLY }, ctx);
    const doc = getSession(id);
    expect(getPcbNodeIds(doc)).toHaveLength(1);
    expect(boardRooted(doc)).toBe(true);
    expect(getPcbNodeIds(throughEngine(doc))).toHaveLength(1);
  });

  it("survives topology_optimize with a domain box on an assembly document", async () => {
    const id = await materializedBoardSession();
    await createCadLoon.handler({ document_id: id, source: ASSEMBLY }, ctx);
    topologyOptimizeTool({ document_id: id, ...domain }, engine);
    expect(getPcbNodeIds(getSession(id))).toHaveLength(1);
  });

  it("keeps the board's placement through a loon rewrite", async () => {
    const id = await materializedBoardSession();
    const before = getSession(id);
    const wrapper = before.roots.find((r) => {
      const op = before.nodes[String(r.root)]!.op as { type: string };
      return op.type === "Translate" && JSON.stringify(before.nodes[String(r.root)]).length > 0 && boardRooted({ ...before, roots: [r] });
    })!;
    (before.nodes[String(wrapper.root)]!.op as unknown as { offset: unknown }).offset = { x: 7, y: 8, z: 9 };
    await createCadLoon.handler({ document_id: id, source: "[cube 1 1 1]" }, ctx);
    const after = getSession(id);
    const offsets = Object.values(after.nodes).flatMap((n) =>
      n.op.type === "Translate" ? [JSON.stringify(n.op.offset)] : [],
    );
    expect(offsets).toContain(JSON.stringify({ x: 7, y: 8, z: 9 }));
  });
});

describe("assembling in place", () => {
  it("adds parts, instances and joints without touching what is there", async () => {
    const id = await materializedBoardSession();
    const before = Object.keys(getSession(id).nodes).length;
    const doc = getSession(id);
    const added = engine.evalLoonDocument('[pub let link [cube 10 10 80]]', {}, {})!;
    addLoonToSession(doc, added);
    expect(Object.keys(doc.nodes).length).toBeGreaterThan(before);
    expect(getPcbNodeIds(doc)).toHaveLength(1);
    const link = doc.roots[doc.roots.length - 1]!;
    expect(doc.nodes[String(link.root)]).toBeDefined();

    const name = doc.nodes[String(link.root)]!.name ?? String(link.root);
    doc.nodes[String(link.root)]!.name = name;
    assembleInto(doc, {
      instances: [{ id: "link1", part: name, at: { x: 0, y: 0, z: 10 } }],
      joints: [{ child: "link1", parent: null, kind: { type: "Revolute", axis: { x: 0, y: 1, z: 0 } } }],
      ground: "link1",
    });
    expect(doc.instances?.map((i) => i.id)).toEqual(["link1"]);
    expect(doc.joints).toHaveLength(1);
    expect(getPcbNodeIds(doc)).toHaveLength(1);
    expect(doc.roots.includes(link)).toBe(false);
    expect(getPcbNodeIds(throughEngine(doc))).toHaveLength(1);
  });

  it("changes nothing when a later entry is invalid", () => {
    const doc = engine.evalLoonDocument('[pub let a [cube 1 1 1]]', {}, {})!;
    const id = registerSession(doc);
    const snapshot = JSON.stringify(getSession(id));
    const name = doc.nodes[String(doc.roots[0]!.root)]!.name ?? String(doc.roots[0]!.root);
    expect(() =>
      assembleInto(getSession(id), {
        instances: [{ id: "i1", part: name }],
        joints: [{ child: "nope", kind: { type: "Fixed" } }],
      }),
    ).toThrow(/Nothing was changed/);
    expect(JSON.stringify(getSession(id))).toBe(snapshot);
  });

  it("finds instances by name after the engine renumbers their ids", () => {
    const doc = engine.evalLoonDocument(ASSEMBLY, {}, {})!;
    (doc.instances ?? []).forEach((i, n) => (i.id = `0:${n}`));
    const added = assembleInto(doc, {
      joints: [{ child: "link1", parent: "tray1", kind: { type: "Fixed" } }],
    });
    expect(added.joints).toHaveLength(1);
    expect(doc.joints![0]!.childInstanceId).toBe("0:1");
  });

  it("refuses to overwrite an instance on add", () => {
    const a = engine.evalLoonDocument(ASSEMBLY, {}, {})!;
    const b = engine.evalLoonDocument(ASSEMBLY, {}, {})!;
    const snapshot = JSON.stringify(a);
    expect(() => addLoonToSession(a, b)).toThrow(/Nothing was changed/);
    expect(JSON.stringify(a)).toBe(snapshot);
  });
});

describe("a mutation pays only for what it changed", () => {
  const plate = "[pub let a [pipe [cube 40 40 4] [difference [pipe [cylinder 5 8] [translate 10 10 -1]]]]] [pub let b [cube 5 5 5]]";

  it("does not re-evaluate an untouched part across an engine rebuild", () => {
    clearIncrementalEvaluation();
    const doc = engine.evalLoonDocument(plate, {}, {})!;
    const before = computeIntegrity(doc, engine)!;
    const seen: string[] = [];
    const real = engine.evaluate.bind(engine);
    const spy = vi.spyOn(engine, "evaluate").mockImplementation((d, o) => {
      seen.push(JSON.stringify(d));
      return real(d, o);
    });
    try {
      const rebuilt = throughEngine(doc);
      // A new cube is the one piece that may need solving.
      const after = computeIntegrity(rebuilt, engine)!;
      expect(seen.filter((s) => s.includes("Difference"))).toHaveLength(0);
      expect(after.volume_mm3).toBeCloseTo(before.volume_mm3 + 8000, 0);
      expect(after.parts).toBe(before.parts + 1);
    } finally {
      spy.mockRestore();
    }
  });

  it("agrees with a full evaluation, assemblies included", () => {
    clearIncrementalEvaluation();
    const doc = engine.evalLoonDocument(ASSEMBLY, {}, {})!;
    const inc = evaluateIncrementally(doc, engine);
    const full = engine.evaluate(doc);
    expect(inc.instances?.length).toBe(full.instances?.length);
    expect(inc.parts.length).toBe(full.parts.length);
  });
});

describe("render output an agent can use", () => {
  it("rasterizes with the WASM build that ships in the tarball", async () => {
    const loaded = await loadResvgWasmForTest();
    if ("reason" in loaded) throw new Error(loaded.reason);
    const svg = '<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="8" height="8" fill="red"/></svg>';
    const png = new loaded.Resvg(svg).render().asPng();
    expect(Buffer.from(png).subarray(1, 4).toString()).toBe("PNG");
  });

  it("hands back a readable file for an SVG over the inline cap", () => {
    const out = oversizedSvg("<svg/>", 999_999, "a/b");
    expect(readFileSync(out.svg_path!, "utf8")).toBe("<svg/>");
  });
});

describe("results say what is true", () => {
  it("fab_prep names the blind-via gate instead of promising export", () => {
    const pcb = { vias: [{ startLayer: "FCu", endLayer: "In1Cu" }], footprints: [], traces: [] };
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const blocked = fabPrepNextStep(pcb as any, false);
    expect(blocked).toMatch(/still refuse/);
    expect(blocked).not.toMatch(/will now pass/);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    expect(fabPrepNextStep({ vias: [], footprints: [], traces: [] } as any, false)).toMatch(/will now pass/);
  });

  const report = () => ({
    ok: true,
    verified: true,
    summary: "fits",
    clearance: 0.5,
    placement: { offset: { x: 0, y: 0, z: 2 }, rotationDeg: 0 },
    checks: [],
  });
  const cavity = { minX: 0, maxX: 30, minY: 0, maxY: 30 };
  const j1 = [{ footprint_ref: "J1", positions: [28, 10, 0, 31, 10, 0] }];

  it("board fit fails when a connector goes through a solid wall", () => {
    const r = report();
    addComponentOverhangCheck(r as never, j1, cavity, false);
    expect(r.ok).toBe(false);
    expect(JSON.stringify(r.checks)).toMatch(/J1 \(1 mm\)/);
  });

  it("board fit only warns when the case has openings", () => {
    const r = report();
    addComponentOverhangCheck(r as never, j1, cavity, true);
    expect(r.ok).toBe(true);
    expect(r.verified).toBe(false);
  });

  it("board_from_solid reports no cutouts in a plain cavity", () => {
    const doc = engine.evalLoonDocument(
      "[pub let box [difference [translate 2 2 2 [cube 76 56 16]] [cube 80 60 20]]]",
      {},
      {},
    )!;
    const id = registerSession(doc);
    const res = out(boardFromSolid({ document_id: id }, engine));
    expect(res.outline_basis).toBe("cavity");
    expect(res.cutouts).toBe(0);
  });

  it("topology_optimize refuses a layout that misses its load", () => {
    const mesh = { positions: [0, 0, 0, 10, 0, 0, 0, 10, 0], indices: [0, 1, 2] };
    const spec = {
      loads: [{ region: { min: [60, 0, 0], max: [60, 10, 10] } }],
      supports: [{ region: { min: [0, 0, 0], max: [0, 10, 10] } }],
    };
    expect(loadPathGap(mesh, spec, 1)).toMatch(/load 1 has no material/);
  });

  it("topology_optimize refuses a load on a detached piece", () => {
    const mesh = {
      positions: [0, 0, 0, 1, 0, 0, 0, 1, 0, 59, 0, 0, 60, 0, 0, 60, 1, 0],
      indices: [0, 1, 2, 3, 4, 5],
    };
    const spec = {
      loads: [{ region: { min: [60, 0, 0], max: [60, 1, 1] } }],
      supports: [{ region: { min: [0, 0, 0], max: [0, 1, 1] } }],
    };
    expect(loadPathGap(mesh, spec, 0.1)).toMatch(/not connected/);
  });

  it("topology_optimize still accepts a connected structure", () => {
    const res = out(topologyOptimizeTool(domain, engine));
    expect(res.part_id).toBeTruthy();
  });
});
