/**
 * Regressions from acceptance run 6: route_nets over imported copper, wall
 * cutouts in a closed case, the loon `root` arity message, and undo after a
 * simulation.
 */
import { describe, it, expect, beforeAll, beforeEach, vi } from "vitest";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { Engine } from "@vcad/engine";
import { getNodePcb, getPcbNodeIds } from "@vcad/core";
import type { Document, Pcb } from "@vcad/ir";
import { createSchematic, placeComponents, routeNets, runDrc, setPlacement } from "../tools/ecad.js";
import { importKicad } from "../tools/import-pcb.js";
import { checkEnclosureFit } from "../tools/enclosure.js";
import { createCadLoon } from "../tools/loon.js";
import { documents, getSession, openDocument } from "../tools/session.js";
import { createServer } from "../server.js";
import { clearIncrementalEvaluation } from "../tools/document-solids.js";
import { poseableParts } from "../tools/sweep.js";

const fakeRoute = vi.hoisted(() => ({ result: null as unknown }));
vi.mock("@vcad/engine", async (original) => {
  const actual = await original<typeof import("@vcad/engine")>();
  return {
    ...actual,
    routeAll: (...args: Parameters<typeof actual.routeAll>) =>
      fakeRoute.result ?? actual.routeAll(...args),
  };
});

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.init();
});
beforeEach(() => {
  documents.clear();
  fakeRoute.result = null;
});

type Out = { content: Array<{ type: string; text?: string }>; isError?: boolean };
const out = (r: Out) => JSON.parse(r.content.find((c) => c.type === "text")!.text!);

const pcbOf = (id: string): Pcb => {
  const doc = getSession(id);
  return getNodePcb(doc, getPcbNodeIds(doc)[0]!)!;
};

const resistor = (ref: string, x: number) => `(footprint "R_0805" (layer "F.Cu") (at ${x} 20)
    (fp_text reference "${ref}" (at 0 0) (layer "F.SilkS"))
    (pad "1" smd rect (at -1 0) (size 1 1.2) (layers "F.Cu") (net 1 "A"))
    (pad "2" smd rect (at 1 0) (size 1 1.2) (layers "F.Cu") (net 2 "B")))`;
const seg = (x1: number, y1: number, x2: number, y2: number, net: number) =>
  `(segment (start ${x1} ${y1}) (end ${x2} ${y2}) (width 0.25) (layer "F.Cu") (net ${net}))`;

/** Two resistors, both nets fully routed around each other. */
const ROUTED_KICAD = `(kicad_pcb (version 20221018) (generator test)
  (general (thickness 1.6))
  (layers (0 "F.Cu" signal) (31 "B.Cu" signal) (44 "Edge.Cuts" user))
  (net 0 "") (net 1 "A") (net 2 "B")
  (gr_line (start 0 0) (end 60 0) (layer "Edge.Cuts") (width 0.05))
  (gr_line (start 60 0) (end 60 40) (layer "Edge.Cuts") (width 0.05))
  (gr_line (start 60 40) (end 0 40) (layer "Edge.Cuts") (width 0.05))
  (gr_line (start 0 40) (end 0 0) (layer "Edge.Cuts") (width 0.05))
  ${resistor("R1", 20)} ${resistor("R2", 40)}
  ${seg(19, 20, 19, 30, 1)} ${seg(19, 30, 39, 30, 1)} ${seg(39, 30, 39, 20, 1)}
  ${seg(21, 20, 21, 12, 2)} ${seg(21, 12, 41, 12, 2)} ${seg(41, 12, 41, 20, 2)}
)`;

async function importRouted(): Promise<string> {
  const content_base64 = Buffer.from(ROUTED_KICAD, "utf8").toString("base64");
  return out(await importKicad({ content_base64 })).document_id;
}
const errorsOf = async (id: string) => out(await runDrc({ document_id: id })).errors;

describe("route_nets on an already-routed imported board", () => {
  it("leaves connected nets on imported copper alone", async () => {
    const id = await importRouted();
    expect(await errorsOf(id)).toBe(0);
    const before = JSON.stringify(pcbOf(id).traces);

    const res = out(await routeNets({ document_id: id }));

    expect(res.success).toBe(true);
    expect(res.already_routed_nets).toEqual(["A", "B"]);
    expect(res.traces_removed).toBeUndefined();
    expect(res.traces_added).toBe(0);
    expect(res.drc).toEqual({ before: 0, after: 0 });
    expect(JSON.stringify(pcbOf(id).traces)).toBe(before);
  });

  it("restores the board and fails when a route makes DRC worse", async () => {
    const id = await importRouted();
    const before = JSON.stringify(pcbOf(id));
    fakeRoute.result = {
      traces: [
        // Net A stays connected, so only the DRC check can catch this: the
        // last segment crosses net B.
        { start: { x: 19, y: 20 }, end: { x: 19, y: 30 }, width: 0.25, layer: "FCu", net: "A" },
        { start: { x: 19, y: 30 }, end: { x: 39, y: 30 }, width: 0.25, layer: "FCu", net: "A" },
        { start: { x: 39, y: 30 }, end: { x: 39, y: 20 }, width: 0.25, layer: "FCu", net: "A" },
        { start: { x: 30, y: 30 }, end: { x: 30, y: 8 }, width: 0.25, layer: "FCu", net: "A" },
      ],
      vias: [],
      zones: [],
      routed_nets: ["A"],
      unrouted_nets: [],
      diagnostics: [],
      routability: 1,
    };

    const raw = await routeNets({ document_id: id, nets: ["A"] });
    const res = out(raw as Out);

    expect(raw.isError).toBe(true);
    expect(res.success).toBe(false);
    expect(res.board_unchanged).toBe(true);
    expect(res.drc.before.errors).toBe(0);
    expect(res.drc.after.errors).toBeGreaterThan(0);
    expect(JSON.stringify(pcbOf(id))).toBe(before);
  });
});

describe("check_enclosure_fit wall openings in a closed case", () => {
  /** 44×44×20 case with a 40×40×16 void, minus each listed box [size, at]. */
  function closedCase(cuts: Array<[number[], number[]]>): Document {
    let next = 1;
    const nodes: Record<string, unknown> = {};
    const box = ([sx, sy, sz]: number[], [x, y, z]: number[]) => {
      const c = next++;
      const t = next++;
      nodes[c] = { id: c, name: null, op: { type: "Cube", size: { x: sx, y: sy, z: sz } } };
      nodes[t] = { id: t, name: null, op: { type: "Translate", child: c, offset: { x, y, z } } };
      return t;
    };
    let body = box([44, 44, 20], [0, 0, 0]);
    for (const [size, at] of [[[40, 40, 16], [2, 2, 2]] as [number[], number[]], ...cuts]) {
      const tool = box(size, at);
      const d = next++;
      nodes[d] = { id: d, name: null, op: { type: "Difference", left: body, right: tool } };
      body = d;
    }
    return {
      version: "0.1",
      nodes,
      materials: {},
      part_materials: {},
      roots: [{ root: body, material: "default" }],
    } as unknown as Document;
  }

  const USB_SLOT: [number[], number[]] = [[6, 12, 8], [40, 16, 4]];
  const SIDE_SLOT: [number[], number[]] = [[6, 10, 6], [-2, 10, 6]];
  const FRONT_SLOT: [number[], number[]] = [[8, 6, 6], [18, -2, 6]];
  // Cut from inside, leaving a 1 mm skin: not a way out.
  const BLIND_POCKET: [number[], number[]] = [[10, 1.5, 5], [10, 41.5, 8]];

  async function board(): Promise<string> {
    const { document_id: id } = out(
      await createSchematic({
        components: [
          { ref: "J1", value: "USB-C", footprint: "USB_C_Receptacle", x: 30, y: 18,
            pins: [{ number: "A6", name: "DP", type: "Bidirectional", x: 0, y: 0 }] },
        ],
        nets: {},
      }),
    );
    out(await placeComponents({ document_id: id, board_width: 36, board_height: 36 }));
    out(await setPlacement({ document_id: id, placements: [{ ref: "J1", x: 36, y: 18 }] }));
    return id;
  }
  const fit = async (cuts: Array<[number[], number[]]>) => {
    const enclosure = out(openDocument({ initial: closedCase(cuts) })).document_id;
    const res = out(await checkEnclosureFit({ document_id: await board(), enclosure_document_id: enclosure }, engine));
    return { res, connector: res.checks.find((c: { id: string }) => c.id === "connector_cutouts") };
  };

  it("finds real cutouts, ignores a blind pocket, and aligns the connector", async () => {
    const { res, connector } = await fit([USB_SLOT, SIDE_SLOT, FRONT_SLOT, BLIND_POCKET]);
    expect(res.cavity_basis).toBe("sealed-void");
    expect(res.openings_detected).toBe(3);
    expect(connector.status).toBe("pass");
  });

  it("still reports a connector that hits a wall away from any cutout", async () => {
    const { res, connector } = await fit([SIDE_SLOT]);
    expect(res.openings_detected).toBe(1);
    expect(connector.status).toBe("fail");
    expect(connector.detail).toMatch(/J1/);
    expect(res.ok).toBe(false);
  });

  it("still reports a connector enclosed by a case with no cutouts", async () => {
    const { res, connector } = await fit([BLIND_POCKET]);
    expect(res.openings_detected).toBe(0);
    expect(connector.status).not.toBe("pass");
    expect(connector.detail).toMatch(/No wall cutouts/);
  });
});

describe("create_cad_loon root without a material", () => {
  it("says what the form needs", () => {
    expect(() =>
      createCadLoon({ source: "[root [translate 0 0 0 [cube 20 20 20]]]" }, engine),
    ).toThrow(/\[root <solid> "<material>"\].*material/s);
  });

  it("does not blame root for an unrelated error", () => {
    expect(() =>
      createCadLoon({ source: '[root [cube 1 nope 1] "steel"]' }, engine),
    ).toThrow(/^(?![\s\S]*<material>)/);
  });
});

describe("evaluating chained cuts", () => {
  it("runs in the WASM evaluator instead of panicking into the TS fallback", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const holes = [5, 15, 25, 35]
      .map((x) => `[translate ${x} 10 -1 [cylinder 2 12]]`)
      .reduce((body, hole) => `[pipe ${body} [difference ${hole}]]`, "[cube 40 20 10]");
    const doc = engine.evalLoonDocument(`[root ${holes} "steel"]`, {})!;

    const scene = engine.evaluate(doc);

    expect(scene.parts.length).toBe(1);

    expect(warn.mock.calls.flat().join(" ")).not.toMatch(/WASM evaluateDocument failed/);
    warn.mockRestore();
  });
});

describe("measuring a part after an unrelated edit", () => {
  it("evaluates only the part that is new", () => {
    clearIncrementalEvaluation();
    const first = engine.evalLoonDocument('[root [cube 10 10 10] "steel"]', {})!;
    poseableParts(first, engine);
    const both = engine.evalLoonDocument(
      '[root [cube 10 10 10] "steel"]\n[root [translate 30 0 0 [cube 5 5 5]] "steel"]',
      {},
    )!;
    const evaluate = vi.spyOn(engine, "evaluate");

    expect(poseableParts(both, engine).length).toBe(2);

    expect(evaluate.mock.calls.length).toBe(1);
    expect(evaluate.mock.calls[0]![0].roots.length).toBe(1);
    evaluate.mockRestore();
  });
});

describe("undo after assemble and a simulation", () => {
  it("rewinds the assemble rather than a step that changed nothing", async () => {
    const server = await createServer(engine, { user: null });
    const client = new Client({ name: "t", version: "1" });
    const [a, b] = InMemoryTransport.createLinkedPair();
    await Promise.all([server.connect(b), client.connect(a)]);
    const call = async (name: string, args: Record<string, unknown>) =>
      (await client.callTool({ name, arguments: args })) as Out;

    const opened = out(await call("open_document", {}));
    const id: string = opened.document_id;
    await call("create_cad_loon", {
      document_id: id,
      mode: "add",
      source: `[let a [cube 20 20 20]]\n[let b [translate 30 0 0 [cube 20 20 20]]]\n[root a "steel"]\n[root b "steel"]`,
    });
    const parts = out(await call("describe_scene", { document_id: id })).parts as Array<{ id: string }>;
    const assembly = {
      document_id: id,
      instances: parts.map((p, i) => ({ id: `i${i}`, part: p.id })),
      joints: [
        { child: "i0", parent: null, kind: { type: "Fixed" } },
        { child: "i1", parent: "i0", kind: { type: "Revolute", axis: { x: 0, y: 0, z: 1 } } },
      ],
      ground: "i0",
    };
    expect((await call("assemble", assembly)).isError).toBeFalsy();
    expect((await call("create_robot_env", { document_id: id, end_effector_ids: ["i1"] })).isError).toBeFalsy();

    const undone = out(await call("undo", { document_id: id }));

    expect(undone.document_changes.instances.removed).toEqual(["i0", "i1"]);
    expect(getSession(id).instances ?? []).toEqual([]);
    expect((await call("assemble", assembly)).isError).toBeFalsy();
  });

  it("does not report an undo when no step changed the document", async () => {
    const server = await createServer(engine, { user: null });
    const client = new Client({ name: "t", version: "1" });
    const [a, b] = InMemoryTransport.createLinkedPair();
    await Promise.all([server.connect(b), client.connect(a)]);
    const call = async (name: string, args: Record<string, unknown>) =>
      (await client.callTool({ name, arguments: args })) as Out;
    const id: string = out(await call("open_document", {})).document_id;
    const failed = await call("assemble", { document_id: id, joints: [{ child: "nope", kind: { type: "Fixed" } }] });
    expect(failed.isError).toBe(true);

    const undone = await call("undo", { document_id: id });

    expect(undone.isError).toBe(true);
    expect(undone.content[0]!.text).toMatch(/nothing to undo/);
  });
});
