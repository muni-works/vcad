/**
 * PCB tools must never report more than they did. Each test pins a result field
 * to what the kernel DRC measures afterwards, on a board where the two used to
 * disagree.
 */
import { describe, it, expect, beforeAll, beforeEach } from "vitest";
import { Engine } from "@vcad/engine";
import { getNodePcb, getPcbNodeIds } from "@vcad/core";
import type { Document, Pcb, Vec2 } from "@vcad/ir";
import {
  addTrace,
  boardFromSolid,
  createSchematic,
  fabPrep,
  fixDrc,
  placeComponents,
  routeNets,
  runDrc,
} from "../tools/ecad.js";
import { documents, openDocument } from "../tools/session.js";

let engine: Engine;

beforeAll(async () => {
  engine = await Engine.init();
});
beforeEach(() => {
  documents.clear();
});

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function out(result: { content: Array<{ type: string; text: string }> }): any {
  return JSON.parse(result.content[0]!.text);
}

const pin = (number: string, name: string) => ({ number, name, type: "Passive" });
const r0805 = (ref: string, x: number, y: number) => ({
  ref,
  value: "1k",
  footprint: "Resistor_SMD:R_0805_2012Metric",
  x,
  y,
  pins: [pin("1", "1"), pin("2", "2")],
});

async function board(): Promise<string> {
  const id = out(
    await createSchematic({
      components: [r0805("R1", 10, 10), r0805("R2", 30, 10), r0805("R3", 10, 25), r0805("R4", 30, 25)],
      nets: { A: ["R1.1", "R2.1"], B: ["R3.1", "R4.1"], C: ["R1.2", "R3.2"] },
    }),
  ).document_id as string;
  out(await placeComponents({ document_id: id, board_width: 50, board_height: 40 }));
  return id;
}

function pcbOf(id: string): Pcb {
  const doc = documents.get(id)!;
  return getNodePcb(doc, getPcbNodeIds(doc)[0]!)!;
}

function padPos(pcb: Pcb, ref: string, number: string): Vec2 {
  const fp = pcb.footprints.find((f) => f.ref === ref)!;
  const pad = fp.pads.find((p) => p.number === number)!;
  return { x: fp.position.x + pad.position.x, y: fp.position.y + pad.position.y };
}

async function openCount(id: string): Promise<number> {
  const drc = out(await runDrc({ document_id: id }));
  return drc.categories.connectivity as number;
}

describe("route_nets reports connectivity as DRC measures it", () => {
  it("counts a net it left open, even one it was told not to touch", async () => {
    const id = await board();
    const r = out(await routeNets({ document_id: id, locked_nets: ["A"] }));
    expect(await openCount(id)).toBe(1);
    expect(r.routability_basis).toBe("drc");
    expect(r.open_nets).toEqual(["A"]);
    expect(r.routability).toBeCloseTo(2 / 3, 6);
    expect(r.nets_routed).toBe(2);
  });

  it("is 1 only when DRC finds nothing open", async () => {
    const id = await board();
    const r = out(await routeNets({ document_id: id }));
    expect(await openCount(id)).toBe(0);
    expect(r.routability).toBe(1);
    expect(r.open_nets).toBeUndefined();
  });

  it("answers a scoped call for the scope, and still names open nets elsewhere", async () => {
    const id = await board();
    const r = out(await routeNets({ document_id: id, nets: ["A"] }));
    expect(r.routability).toBe(1);
    expect([...r.open_nets].sort()).toEqual(["B", "C"]);
  });

  it("counts copper from the board, not from the passes that laid it", async () => {
    const id = await board();
    const before = pcbOf(id).traces.length;
    const r = out(await routeNets({ document_id: id }));
    expect(r.traces_added).toBe(pcbOf(id).traces.length - before);
  });
});

describe("fix_drc closes what it can and names the rest", () => {
  it("closes an open net around its hand-placed copper", async () => {
    const id = await board();
    out(await routeNets({ document_id: id, locked_nets: ["A"] }));
    const pcb = pcbOf(id);
    const from = padPos(pcb, "R1", "1");
    const stub = { x: from.x, y: from.y + 1.5 };
    out(
      await addTrace({
        document_id: id,
        net: "A",
        layer: "FCu",
        width: pcb.rules.defaultRules.traceWidth,
        points: [from, stub],
      }),
    );
    const manualBefore = pcbOf(id).traces.filter((t) => t.source === "manual");
    expect(manualBefore.length).toBeGreaterThan(0);

    const r = out(await fixDrc({ document_id: id }));
    expect(r.fixed.map((f: { action: string }) => f.action)).toContain("connect_net");
    expect(r.clean).toBe(true);
    expect(r.remaining).toEqual([]);
    expect(await openCount(id)).toBe(0);
    const manualAfter = pcbOf(id).traces.filter((t) => t.source === "manual");
    expect(manualAfter).toEqual(manualBefore);
  });

  it("says what remains and why when it can't fix something", async () => {
    const id = await board();
    out(await routeNets({ document_id: id }));
    const pcb = pcbOf(id);
    const c = pcb.footprints[0]!.position;
    pcb.keepouts = [
      ...(pcb.keepouts ?? []),
      {
        outline: [
          { x: c.x - 3, y: c.y - 3 },
          { x: c.x + 3, y: c.y - 3 },
          { x: c.x + 3, y: c.y + 3 },
          { x: c.x - 3, y: c.y + 3 },
        ],
        layers: ["FCu"],
        noTracks: false,
        noVias: false,
        noPour: false,
        noComponents: true,
      },
    ];
    const r = out(await fixDrc({ document_id: id }));
    expect(r.clean).toBe(false);
    const keepout = r.remaining.find((x: { rule: string }) => x.rule === "Keepout");
    expect(keepout.count).toBe(1);
    expect(keepout.reasons[0]).toMatch(/set_placement/);
    expect(keepout.sample_message).toMatch(/inside a no-components keepout/);
  });
});

describe("fab_prep reports one number", () => {
  it("does not fail a connected board over dead copper it then prunes", async () => {
    const id = await board();
    out(await routeNets({ document_id: id }));
    const pcb = pcbOf(id);
    const net = pcb.traces[0]!.net;
    pcb.traces.push({
      start: { x: 2, y: 2 },
      end: { x: 4, y: 2 },
      width: pcb.rules.defaultRules.traceWidth,
      layer: "FCu",
      net,
    });
    const r = out(await fabPrep({ document_id: id }));
    expect(r.converged).toBe(true);
    expect(r.pruned.traces).toBe(1);
    expect(r.copper.traces_after).toBe(r.copper.traces_before - 1);
    expect(await openCount(id)).toBe(0);
  });

  it("quotes the open-net count run_drc gives afterwards", async () => {
    const id = await board();
    const r = out(await fabPrep({ document_id: id, route_remaining: false }));
    const open = await openCount(id);
    expect(open).toBe(3);
    expect(r.converged).toBe(false);
    expect(r.connectivity.open_nets).toHaveLength(open);
    expect(r.connectivity.on_completion).toBe(open);
    expect(r.headline).toContain(`${open} net(s) open`);
    expect(r.headline).not.toContain("more net");
  });
});

describe("board_from_solid", () => {
  function session(root: object, extra: Record<string, object> = {}): string {
    const doc = {
      version: "0.1",
      nodes: { "1": { id: 1, name: null, op: root }, ...extra },
      materials: {},
      part_materials: {},
      roots: [{ root: 1, material: "default" }],
    } as unknown as Document;
    return out(openDocument({ initial: doc })).document_id as string;
  }
  const cube = (x: number, y: number, z: number) => ({ type: "Cube", size: { x, y, z } });
  const node = (id: number, op: object) => ({ id, name: null, op });
  // A 100x80x30 box, open on top, with 3 mm walls and floor.
  const tray = () =>
    session(
      { type: "Difference", left: 2, right: 3 },
      {
        "2": node(2, cube(100, 80, 30)),
        "3": node(3, { type: "Translate", child: 4, offset: { x: 3, y: 3, z: 3 } }),
        "4": node(4, cube(94, 74, 40)),
      },
    );

  it("returns an enclosure's cavity, and says so", () => {
    const r = out(boardFromSolid({ document_id: tray() }, engine));
    expect(r.outline_basis).toBe("cavity");
    expect(r.area_mm2).toBeGreaterThan(94 * 74 * 0.97);
    expect(r.area_mm2).toBeLessThan(94 * 74 * 1.03);
    expect(r.note).toMatch(/cavity/);
  });

  it("returns the silhouette when asked, and says so", () => {
    const r = out(boardFromSolid({ document_id: tray(), region: "outer" }, engine));
    expect(r.outline_basis).toBe("outer");
    expect(r.area_mm2).toBeGreaterThan(100 * 80 * 0.97);
  });

  it("keeps the silhouette of a plain plate and refuses to invent a cavity", () => {
    const id = session(cube(60, 40, 1.6));
    const r = out(boardFromSolid({ document_id: id }, engine));
    expect(r.outline_basis).toBe("outer");
    expect(r.note).toMatch(/no interior cavity/);
    expect(() => boardFromSolid({ document_id: id, region: "cavity" }, engine)).toThrow(/no interior cavity/);
  });

  it("does not mistake a through-hole for a cavity", () => {
    const id = session(
      { type: "Difference", left: 2, right: 3 },
      {
        "2": node(2, cube(60, 40, 10)),
        "3": node(3, { type: "Translate", child: 4, offset: { x: 5, y: 5, z: -5 } }),
        "4": node(4, cube(50, 30, 20)),
      },
    );
    const r = out(boardFromSolid({ document_id: id }, engine));
    expect(r.outline_basis).toBe("outer");
    expect(r.cutouts).toBe(1);
  });
});
