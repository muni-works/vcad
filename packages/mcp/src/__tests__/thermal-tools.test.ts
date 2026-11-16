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

  it("Radiation boundary: vacuum part sheds its power by radiation", async () => {
    // A 10 mm cube in vacuum, 1 W, radiating from all faces (eps 0.6):
    // with no other path, every watt must leave by radiation, and the
    // surface lands on (P/(eps*sigma*A) + Tamb^4)^(1/4).
    const radSpec = {
      origin_mm: [0, 0, 0],
      size_mm: [10, 10, 10],
      divisions: [5, 5, 5],
      materials: [
        {
          shape: { type: "Box", min_mm: [0, 0, 0], size_mm: [10, 10, 10] },
          k_w_mk: [100, 100, 100],
        },
      ],
      sources: [
        {
          name: "p",
          shape: { type: "Box", min_mm: [0, 0, 0], size_mm: [10, 10, 10] },
          power_w: 1.0,
        },
      ],
      domain_faces: Array(6).fill({
        type: "Radiation",
        emissivity: "eps",
        ambient_c: 25,
      }),
    };
    let res: { isError?: boolean; content: Array<{ text: string }> };
    res = (await client.callTool({
      name: "solve_thermal",
      arguments: { spec: radSpec, parameters: { eps: 0.6 } },
    })) as typeof res;
    // A kernel WASM that predates the radiation BC rejects the variant;
    // skip rather than fail (CI builds WASM from source).
    if (res.isError && res.content[0].text.includes("unknown variant")) return;
    expect(res.isError).toBeFalsy();
    const out = JSON.parse(res.content[0].text);
    expect(out.newton_iterations).toBeGreaterThan(0);
    expect(out.radiation.length).toBe(6);
    expect(Math.abs(out.energy.radiation_out_w - 1.0)).toBeLessThan(1e-6);
    const sigma = 5.670374419e-8;
    const ts =
      Math.pow(1.0 / (0.6 * sigma * 6e-4) + Math.pow(298.15, 4), 0.25) -
      273.15;
    expect(Math.abs(out.radiation[0].mean_surface_c - ts)).toBeLessThan(0.1);
    const names = out.claim_set.claims.map((c: { name: string }) => c.name);
    expect(names).toContain("radiation_fraction");
    expect(out.claim_set.provenance.radiation_model).toContain("gray-diffuse");

    // Emissivity out of range fails closed.
    res = (await client.callTool({
      name: "solve_thermal",
      arguments: { spec: radSpec, parameters: { eps: 1.5 } },
    })) as typeof res;
    expect(res.isError).toBe(true);
    expect(res.content[0].text).toContain("emissivity");
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
