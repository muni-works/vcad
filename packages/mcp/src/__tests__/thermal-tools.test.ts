import { describe, it, expect, beforeAll } from "vitest";
import { Engine } from "@vcad/engine";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { createServer } from "../server.js";
import { documents } from "../tools/session.js";

/**
 * End-to-end coverage for the thermal solve tool: the real server, the real
 * WASM kernel (CI builds it from source), a coarse chip-on-board grid.
 *
 * Skips (rather than fails) when the loaded kernel WASM predates the
 * thermal bindings. Locally: `npm run build -w vcad-kernel-wasm`.
 */

const probeEngine = await Engine.init();
const wasmHasThermal =
  typeof (
    probeEngine as unknown as {
      kernel?: { thermalSolve?: unknown };
    }
  ).kernel?.thermalSolve === "function";

const SPEC = {
  origin_mm: [0, 0, 0],
  size_mm: [40, 40, 1.6],
  divisions: [20, 20, 2],
  materials: [
    {
      shape: { type: "Box", min_mm: [0, 0, 0], size_mm: [40, 40, 1.6] },
      k_w_mk: [15, 15, 0.5],
    },
  ],
  sources: [
    {
      name: "die",
      shape: { type: "Box", min_mm: [15, 15, 0], size_mm: [10, 10, 1.6] },
      power_w: "p_die",
    },
  ],
  domain_faces: [
    { type: "Adiabatic" },
    { type: "Adiabatic" },
    { type: "Adiabatic" },
    { type: "Adiabatic" },
    { type: "Convection", h_w_m2k: 12, ambient_c: 25 },
    { type: "Convection", h_w_m2k: 12, ambient_c: 25 },
  ],
};

type ToolText = { content: Array<{ type: string; text: string }> };

describe.skipIf(!wasmHasThermal)("thermal solve tool", () => {
  let client: Client;

  beforeAll(async () => {
    documents.clear();
    const engine = await Engine.init();
    const server = await createServer(engine, { user: null });
    const [clientT, serverT] = InMemoryTransport.createLinkedPair();
    client = new Client(
      { name: "thermal-tools", version: "0.0.0" },
      { capabilities: {} },
    );
    await Promise.all([client.connect(clientT), server.connect(serverT)]);
  });

  it("solve_thermal returns T_max, theta, energy audit + provisional claims", async () => {
    const res = (await client.callTool({
      name: "solve_thermal",
      arguments: {
        spec: SPEC,
        parameters: { p_die: 2.0 },
      },
    })) as ToolText;
    const out = JSON.parse(res.content[0].text);

    // 2 W into a small board over h=12 convection: well above ambient.
    expect(out.t_max_c).toBeGreaterThan(30);
    expect(out.sources.length).toBe(1);
    expect(out.sources[0].name).toBe("die");
    expect(out.sources[0].theta_c_per_w).toBeGreaterThan(0);
    // Energy balance closes.
    expect(Math.abs(out.energy.residual_rel)).toBeLessThan(1e-6);

    expect(out.claim_set.schema).toBe("vcad.thermal-claims/1");
    const names = out.claim_set.claims.map((c: { name: string }) => c.name);
    expect(names).toContain("t_max_c");
    expect(names).toContain("theta_ja_c_per_w");
    expect(names).toContain("energy_balance_residual");

    expect(out.receipt_claims.length).toBe(out.claim_set.claims.length);
    for (const c of out.receipt_claims) {
      expect(c.domain).toBe("thermal");
      expect(c.id.startsWith("thermal.")).toBe(true);
      expect(c.basis).toBe("predicted");
    }
  });

  it("transient mode returns time series, schedule switching + audit", async () => {
    const wasmHasTransient =
      typeof (
        probeEngine as unknown as {
          kernel?: { thermalSolveTransient?: unknown };
        }
      ).kernel?.thermalSolveTransient === "function";
    if (!wasmHasTransient) return; // WASM predates the transient binding

    const spec = {
      ...SPEC,
      materials: [
        { ...SPEC.materials[0], heat_capacity_j_m3k: 1.8e6 },
      ],
    };
    const res = (await client.callTool({
      name: "solve_thermal",
      arguments: {
        spec,
        parameters: { p_die: 2.0 },
        transient: {
          initial_c: 25.0,
          segments: [
            { duration_s: 60, dt_s: 3, source_power_w: { die: 2.0 } },
            { duration_s: 60, dt_s: 3, source_power_w: { die: 0.0 } },
          ],
        },
      },
    })) as ToolText;
    const out = JSON.parse(res.content[0].text);

    expect(out.times_s.length).toBe(40);
    expect(out.t_max_c.length).toBe(40);
    expect(out.source_names).toEqual(["die"]);
    // Heats while powered, cools after the switch.
    expect(out.t_max_c[19]).toBeGreaterThan(25.5);
    expect(out.t_max_c[39]).toBeLessThan(out.t_max_c[19]);
    expect(out.energy_audit_residual_rel).toBeLessThan(1e-3);

    expect(out.claim_set.schema).toBe("vcad.thermal-claims/1");
    const names = out.claim_set.claims.map((c: { name: string }) => c.name);
    expect(names).toContain("t_max_peak_c");
    expect(names).toContain("t_max_final_c");
    expect(names).toContain("transient_energy_audit_residual");
    for (const c of out.receipt_claims) {
      expect(c.basis).toBe("predicted");
    }
  });

  it("transient without heat capacity fails closed", async () => {
    const wasmHasTransient =
      typeof (
        probeEngine as unknown as {
          kernel?: { thermalSolveTransient?: unknown };
        }
      ).kernel?.thermalSolveTransient === "function";
    if (!wasmHasTransient) return;

    const res = (await client.callTool({
      name: "solve_thermal",
      arguments: {
        spec: SPEC,
        parameters: { p_die: 2.0 },
        transient: {
          initial_c: 25.0,
          segments: [{ duration_s: 10, dt_s: 1 }],
        },
      },
    })) as { isError?: boolean; content: Array<{ text: string }> };
    expect(res.isError).toBe(true);
    expect(res.content[0].text).toContain("heat capacity");
  });

  it("an all-adiabatic (ungrounded) domain fails closed", async () => {
    const res = (await client.callTool({
      name: "solve_thermal",
      arguments: {
        spec: {
          ...SPEC,
          sources: [
            {
              name: "die",
              shape: {
                type: "Box",
                min_mm: [15, 15, 0],
                size_mm: [10, 10, 1.6],
              },
              power_w: 2,
            },
          ],
          domain_faces: [
            { type: "Adiabatic" },
            { type: "Adiabatic" },
            { type: "Adiabatic" },
            { type: "Adiabatic" },
            { type: "Adiabatic" },
            { type: "Adiabatic" },
          ],
        },
      },
    })) as { isError?: boolean; content: Array<{ text: string }> };
    expect(res.isError).toBe(true);
  });
});

/**
 * `solve_thermal` with `document_id`: the board and die are parts of a loon
 * document, voxelized by `vcad_eval::thermal_extract`; the die's power is
 * the document's own `amp_loss_w` parameter.
 */
const wasmHasThermalDoc =
  typeof (
    probeEngine as unknown as {
      kernel?: { thermalSolveDocument?: unknown };
    }
  ).kernel?.thermalSolveDocument === "function";

const BOARD_LOON =
  "[defparam amp_loss_w 0.8]\n" +
  "[let board [translate -25 -25 43 [cube 50 50 1.6]]]\n[root board \"fr4\"]\n" +
  "[let die [translate -2.5 -2.5 44.6 [cube 5 5 1]]]\n[root die \"silicon\"]\n";

const conv = (h: number) => ({ type: "convection", hWM2k: h, ambientC: 25 });
const STUDY = {
  parts: ["board", "die"],
  divisions: [50, 50, 13],
  materials: [
    { part: "board", laminate: { inPlaneWMk: 15, throughPlaneWMk: 0.4 } },
    { part: "die", kWMk: [120] },
  ],
  sources: [{ name: "amp", part: "die", powerW: "amp_loss_w" }],
  exposed: conv(5),
  domainFaces: [
    { type: "adiabatic" },
    { type: "adiabatic" },
    { type: "adiabatic" },
    { type: "adiabatic" },
    conv(5),
    conv(5),
  ],
};

type ToolOut = { isError?: boolean; content: Array<{ type: string; text: string }> };

describe.skipIf(!wasmHasThermalDoc)("solve_thermal on a document", () => {
  let call: (name: string, args: Record<string, unknown>) => Promise<ToolOut>;
  let id: string;

  beforeAll(async () => {
    documents.clear();
    const engine = await Engine.init();
    const server = await createServer(engine, { user: null });
    const [clientT, serverT] = InMemoryTransport.createLinkedPair();
    const client = new Client({ name: "thermal-doc", version: "0.0.0" }, { capabilities: {} });
    await Promise.all([client.connect(clientT), server.connect(serverT)]);
    call = async (name, args) => (await client.callTool({ name, arguments: args })) as ToolOut;
    id = JSON.parse((await call("open_document", {})).content[0].text).document_id;
    const created = await call("create_cad_loon", { document_id: id, mode: "add", source: BOARD_LOON });
    expect(created.isError, created.content[0]?.text).toBeFalsy();
  });

  it("voxelizes the parts: the amp-on-board on the every-physics PoC grid", async () => {
    const res = await call("solve_thermal", { document_id: id, study: STUDY });
    expect(res.isError, res.content[0].text).toBeFalsy();
    const out = JSON.parse(res.content[0].text);
    // Same answer as the Rust puck test on this grid (82.53 °C). The PoC's
    // hand painting printed 80.6: its closed boxes give the die a 36 mm²
    // footprint where the 1/2-fill voxelizer gives 32 (corners a quarter
    // full) — see docs/thermal-voxelizer.md.
    expect(out.t_max_c).toBeCloseTo(82.53, 1);
    expect(out.sources[0].name).toBe("amp");
    expect(Math.abs(out.energy.residual_rel)).toBeLessThan(1e-6);
    expect(out.extracted.document_id).toBe(id);
    expect(out.extracted.grid.divisions).toEqual([50, 50, 13]);
    const [board, die] = out.extracted.parts;
    expect(board.voxels).toBe(50 * 50 * 8);
    expect(board.laminate_axis).toBe(2);
    expect(die.voxels).toBe((36 - 4) * 5);
    expect(out.extracted.parameters.amp_loss_w).toBe(0.8);
    for (const c of out.receipt_claims) expect(c.basis).toBe("predicted");
  });

  it("caller parameters override the document's (linear in power)", async () => {
    const a = JSON.parse((await call("solve_thermal", { document_id: id, study: STUDY })).content[0].text);
    const b = JSON.parse(
      (await call("solve_thermal", { document_id: id, study: STUDY, parameters: { amp_loss_w: 1.6 } }))
        .content[0].text,
    );
    expect((b.t_max_c - 25) / (a.t_max_c - 25)).toBeCloseTo(2, 6);
  });

  it("fails closed on a too-coarse grid, naming the part", async () => {
    const res = await call("solve_thermal", {
      document_id: id,
      study: { ...STUDY, divisions: [50, 50, 3] },
    });
    expect(res.isError).toBe(true);
    expect(res.content[0].text).toContain('"board"');
    expect(res.content[0].text).toContain("thick along z");
  });

  it("without a study, says the document has none", async () => {
    const res = await call("solve_thermal", { document_id: id });
    expect(res.isError).toBe(true);
    expect(res.content[0].text).toContain("no thermal analysis study");
  });
});
