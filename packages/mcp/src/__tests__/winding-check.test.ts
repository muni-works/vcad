import { describe, it, expect, beforeAll, beforeEach } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { Engine } from "@vcad/engine";
import type { Document, Pcb } from "@vcad/ir";
import {
  addMotorWinding,
  checkWinding,
  createSchematic,
  placeComponents,
  runDrc,
  setBoardOutline,
  setDesignRules,
  setPlacement,
  setStackup,
} from "../tools/ecad.js";
import {
  checkWindingContinuity,
  type WindingCheckSpec,
} from "../tools/winding-check.js";
import { documents, getSession } from "../tools/session.js";

// The pcb-motor stator v3 (examples/pcb-motor) shipped with the first coil of
// PHB and of PHC open: add_motor_winding could not route those two feeds yet
// reported success, and the pipeline's "feed stitch" tied each pad to the
// series ring AFTER the first coil. DRC was 0 throughout. These tests pin the
// exact pipeline geometry: the realizer must route all four feeds, and the
// winding check must tell a live coil from a dead one.

beforeAll(async () => {
  await Engine.init();
});
beforeEach(() => documents.clear());

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const out = (r: { content: Array<{ type: string; text: string }> }): any =>
  JSON.parse(r.content[0]!.text);

const boardOf = (id: string): Pcb => {
  const doc = getSession(id) as Document;
  const node = Object.values(doc.nodes).find(
    (n) => (n.op as { type: string }).type === "PcbBoard",
  );
  return (node!.op as unknown as { board: Pcb }).board;
};

const WINDING = {
  slots: 9,
  poles: 6,
  phases: 3,
  center: { x: 35, y: 35 },
  pitch_radius: 22.5,
  inner_radius: 2.6,
  outer_radius: 7.2,
  trace_width: 0.25,
  clearance: 0.2,
  turns_per_coil: 10,
  connection: "wye",
  copper_layer: "FCu",
  return_layer: "BCu",
  neutral_net: "WIND_N",
  phase_nets: ["PHA", "PHB", "PHC"],
};

/** scripts/pipeline.py B1–B2, verbatim: JST-XH-4 at r≈32 inside the escape band. */
async function statorV3(): Promise<{
  id: string;
  winding: Record<string, unknown>;
}> {
  const sch = out(
    await createSchematic({
      title: "PCB Stator v3",
      components: [
        {
          ref: "J1",
          footprint: "JST_XH_4",
          value: "B4B-XH-A",
          x: 10,
          y: 10,
          pins: [
            { number: "1", name: "PHA", type: "Passive" },
            { number: "2", name: "PHB", type: "Passive" },
            { number: "3", name: "PHC", type: "Passive" },
            { number: "4", name: "WIND_N", type: "Passive" },
          ],
        },
      ],
      nets: { PHA: ["J1.1"], PHB: ["J1.2"], PHC: ["J1.3"], WIND_N: ["J1.4"] },
    }),
  );
  const id = sch.document_id as string;
  await placeComponents({
    document_id: id,
    board_shape: { type: "circle", outer_diameter: 70, inner_diameter: 10 },
    board_thickness: 1.6,
    strategy: "radial",
    radial_radius: 32,
    radial_start_angle_deg: 100,
  });
  await setStackup({ document_id: id, copper_oz: 2 });
  setDesignRules({
    document_id: id,
    clearance: 0.2,
    track_width: 0.25,
    via_drill: 0.3,
    via_diameter: 0.6,
    edge_clearance: 0.3,
    min_drill: 0.3,
    min_annular_ring: 0.13,
  });
  await setPlacement({
    document_id: id,
    placements: [{ ref: "J1", x: 29.44, y: 66.51, rotation: 190 }],
  });
  const winding = out(await addMotorWinding({ document_id: id, ...WINDING }));
  return { id, winding };
}

const checkArgs = (id: string) => ({
  document_id: id,
  slots: 9,
  poles: 6,
  center: { x: 35, y: 35 },
  pitch_radius: 22.5,
  outer_radius: 7.2,
  turns_per_coil: 10,
});

describe("pcb-motor stator v3: every coil carries current", () => {
  it("add_motor_winding routes all four feeds into the connector band", async () => {
    const { id, winding } = await statorV3();
    expect(winding.feeds_unrouted).toBeUndefined();
    expect(winding.feeds_routed).toEqual(
      expect.arrayContaining(["PHA", "PHB", "PHC", "WIND_N"]),
    );
    expect(winding.errors).toBeUndefined();
    expect(winding.success).toBe(true);
    // The realizer ran the current check itself.
    const wc = winding.winding_check as {
      pass: boolean;
      coils: Array<{ ampere_turns_per_amp: number }>;
    };
    expect(wc.pass).toBe(true);
    expect(wc.coils).toHaveLength(9);

    // check_winding agrees: 9 coils, |AT| ≈ 10 per amp, one common sense.
    const res = await checkWinding(checkArgs(id));
    expect((res as { isError?: boolean }).isError).toBeUndefined();
    const r = out(res);
    expect(r.pass).toBe(true);
    for (const c of r.coils) {
      expect(Math.abs(c.ampere_turns_per_amp)).toBeGreaterThan(9);
      expect(Math.abs(c.ampere_turns_per_amp)).toBeLessThan(11);
      expect(Math.sign(c.ampere_turns_per_amp) * c.polarity).toBe(r.sense);
    }
    for (const p of r.phases) expect(p.dead_copper_mm).toBe(0);

    // And it is still DRC-clean once the bore-mount outline is cut.
    const ng = (cx: number, cy: number, rr: number, n: number) =>
      Array.from({ length: n }, (_, i) => ({
        x: +(cx + rr * Math.cos((2 * Math.PI * i) / n)).toFixed(4),
        y: +(cy + rr * Math.sin((2 * Math.PI * i) / n)).toFixed(4),
      }));
    const P = (rr: number, d: number): [number, number] => [
      35 + rr * Math.cos((d * Math.PI) / 180),
      35 + rr * Math.sin((d * Math.PI) / 180),
    ];
    await setBoardOutline({
      document_id: id,
      thickness: 1.6,
      outline: {
        vertices: ng(35, 35, 35, 64),
        cutouts: [
          ng(35, 35, 5, 32),
          ...[60, 180, 300].map((a) => ng(...P(8, a), 1.7, 16)),
        ],
      },
    });
    const drc = out(await runDrc({ document_id: id }));
    expect(drc.violations).toBe(0);
  }, 120_000);

  it("check_winding fails the old 'feed stitch' repair, naming the dead coils", async () => {
    const { id } = await statorV3();
    const pcb = boardOf(id);
    // Re-create the shipped defect: the PHB/PHC feed never reaches the first
    // coil's centre via (drop it), and the pad is stitched onto the series
    // ring past that coil instead — pipeline.py's old repair, verbatim.
    const firstCoil = {
      PHB: { x: 52.236, y: 49.463 },
      PHC: { x: 38.907, y: 57.158 },
    } as const;
    for (const [net, c] of Object.entries(firstCoil)) {
      const i = pcb.vias.findIndex(
        (v) => v.net === net && v.position.x === c.x && v.position.y === c.y,
      );
      expect(i).toBeGreaterThanOrEqual(0);
      pcb.vias.splice(i, 1);
    }
    const P = (rr: number, d: number) => ({
      x: Math.round((35 + rr * Math.cos((d * Math.PI) / 180)) * 1000) / 1000,
      y: Math.round((35 + rr * Math.sin((d * Math.PI) / 180)) * 1000) / 1000,
    });
    for (const [net, pad, ringR, ang] of [
      ["PHB", { x: 30.671, y: 66.727 }, 13.8, 97.8],
      ["PHC", { x: 28.209, y: 66.293 }, 12.9, 102.2],
    ] as const) {
      const hop = P(16, ang);
      const ring = P(ringR, ang);
      pcb.traces.push({
        start: pad,
        end: hop,
        width: 0.5,
        layer: "BCu",
        net,
        source: "manual",
      } as never);
      pcb.traces.push({
        start: hop,
        end: ring,
        width: 0.5,
        layer: "BCu",
        net,
        source: "manual",
      } as never);
      for (const at of [hop, ring]) {
        pcb.vias.push({
          position: at,
          diameter: 0.6,
          drill: 0.3,
          startLayer: "FCu",
          endLayer: "BCu",
          net,
          source: "manual",
        } as never);
      }
    }

    const res = await checkWinding(checkArgs(id));
    expect((res as { isError?: boolean }).isError).toBe(true);
    const r = out(res);
    expect(r.pass).toBe(false);
    const dead = r.coils.filter((c: { ok: boolean }) => !c.ok);
    expect(dead.map((c: { center: unknown }) => c.center)).toEqual(
      expect.arrayContaining([firstCoil.PHB, firstCoil.PHC]),
    );
    for (const c of dead) expect(c.problem).toContain("open coil");
    // Every other coil still carries its full ten ampere-turns.
    expect(r.coils.filter((c: { ok: boolean }) => c.ok)).toHaveLength(7);
    expect(r.failures.join("\n")).toMatch(
      /PHB @ \(52\.236, 49\.463\): carries no current/,
    );
  }, 120_000);
});

describe("examples/pcb-motor/stator-v3.vcad (the committed board)", () => {
  it("passes the winding check: 9 live coils, 3 per phase", () => {
    const doc = JSON.parse(
      readFileSync(
        resolve(__dirname, "../../../../examples/pcb-motor/stator-v3.vcad"),
        "utf8",
      ),
    ) as Document;
    const node = Object.values(doc.nodes).find(
      (n) => (n.op as { type: string }).type === "PcbBoard",
    )!;
    const pcb = (node.op as unknown as { board: Pcb }).board;
    const nets = ["PHA", "PHB", "PHC"] as const;
    const spec: WindingCheckSpec = {
      coils: Array.from({ length: 9 }, (_, k) => {
        const a = (k * 40 * Math.PI) / 180;
        return {
          center: {
            x: Math.round((35 + 22.5 * Math.cos(a)) * 1000) / 1000,
            y: Math.round((35 + 22.5 * Math.sin(a)) * 1000) / 1000,
          },
          net: nets[k % 3]!,
          polarity: 1 as const,
        };
      }),
      outerRadius: 7.2,
      turns: 10,
      copperLayer: "FCu",
      neutralNet: "WIND_N",
    };
    const r = checkWindingContinuity(pcb, spec);
    expect(r.failures).toEqual([]);
    expect(r.pass).toBe(true);
    for (const net of nets) {
      expect(r.coils.filter((c) => c.net === net && c.ok)).toHaveLength(3);
    }
  });
});
