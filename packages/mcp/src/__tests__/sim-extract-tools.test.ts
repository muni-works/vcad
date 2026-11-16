import { describe, it, expect, beforeAll } from "vitest";
import { Engine } from "@vcad/engine";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { createServer } from "../server.js";
import { documents } from "../tools/session.js";

/**
 * CAD → simulation over MCP: `simulate_charged_particles` and
 * `simulate_neutron_shield` taking a `document_id` + part selection
 * instead of a hand-written spec. The fusor is authored in loon (6"
 * chamber, 4-ring 1.5" grid, 15 cm HDPE shell), the specs are measured from
 * its evaluated solids by `vcad_eval::sim_extract`, and the potential /
 * source rate stay named document parameters.
 *
 * Skips when the loaded kernel WASM predates the extraction bindings (the
 * checked-in artifacts refresh only on main; CI builds from source).
 */

const probeEngine = await Engine.init();
const wasmHasExtract =
  typeof (
    probeEngine as unknown as {
      kernel?: { extractDeviceSpec?: unknown };
    }
  ).kernel?.extractDeviceSpec === "function";

const FUSOR = "[defparam cathode_v -20000.0]\n[defparam src_rate 25000.0]\n[root-named [translate 0 0 -79.2 [difference [translate 0 0 3 [cylinder 76.2 152.4]] [cylinder 79.2 158.4]]] \"steel\" \"chamber\"]\n[root-named [union [translate 0 0 15.411773742843 [torus 11.197309056172 1]] [translate 0 0 5.886773742843 [torus 18.117626635423 1]] [translate 0 0 -5.886773742843 [torus 18.117626635423 1]] [translate 0 0 -15.411773742843 [torus 11.197309056172 1]]] \"tungsten\" \"grid\"]\n[root-named [difference [sphere 150] [sphere 300]] \"hdpe\" \"shield\"]\n[root-named [rotate 90 0 0 [torus 19.05 1]] \"tungsten\" \"meridian\"]";

type ToolOut = { isError?: boolean; content: Array<{ type: string; text: string }> };

describe.skipIf(!wasmHasExtract)("document → simulation extraction tools", () => {
  let call: (name: string, args: Record<string, unknown>) => Promise<ToolOut>;
  let id: string;

  beforeAll(async () => {
    documents.clear();
    const engine = await Engine.init();
    const server = await createServer(engine, { user: null });
    const [clientT, serverT] = InMemoryTransport.createLinkedPair();
    const client = new Client({ name: "sim-extract", version: "0.0.0" }, { capabilities: {} });
    await Promise.all([client.connect(clientT), server.connect(serverT)]);
    call = async (name, args) => (await client.callTool({ name, arguments: args })) as ToolOut;
    id = JSON.parse((await call("open_document", {})).content[0].text).document_id;
    const created = await call("create_cad_loon", { document_id: id, mode: "add", source: FUSOR });
    expect(created.isError, created.content[0]?.text).toBeFalsy();
  });

  const extract = {
    chamber: "chamber",
    electrodes: [{ part: "grid", potential: "cathode_v" }],
  };

  it("simulate_charged_particles runs on the document's own geometry", async () => {
    const res = await call("simulate_charged_particles", {
      document_id: id,
      extract,
      options: { nr: 41, nz: 81, particles: 6, max_passes: 4 },
    });
    expect(res.isError, res.content[0].text).toBeFalsy();
    const out = JSON.parse(res.content[0].text);
    expect(out.stats.n).toBe(6);
    expect(out.extracted.document_id).toBe(id);
    expect(out.extracted.ring_parts).toEqual(["grid", "grid", "grid", "grid"]);
    expect(out.extracted.spec.chamber_radius_mm).toBeCloseTo(76.2, 9);
    expect(out.extracted.spec.rings[0].potential_v).toBe("cathode_v");
    expect(out.extracted.parameters.cathode_v).toBe(-20000);
    for (const r of out.extracted.spec.rings) {
      expect(Math.hypot(r.ring_radius_mm, r.z_mm)).toBeCloseTo(19.05, 6);
    }
  });

  it("caller parameters override the document's", async () => {
    const res = await call("simulate_charged_particles", {
      document_id: id,
      extract,
      parameters: { cathode_v: -30000 },
      options: { nr: 41, nz: 81, particles: 4, max_passes: 2 },
    });
    expect(res.isError, res.content[0].text).toBeFalsy();
    expect(JSON.parse(res.content[0].text).extracted.parameters.cathode_v).toBe(-30000);
  });

  it("a meridian ring fails closed, naming the part", async () => {
    const res = await call("simulate_charged_particles", {
      document_id: id,
      extract: { ...extract, electrodes: [{ part: "meridian", potential: "cathode_v" }] },
    });
    expect(res.isError).toBe(true);
    expect(res.content[0].text).toContain("meridian");
    expect(res.content[0].text).toContain("not coaxial");
  });

  it("simulate_neutron_shield runs on the document's shell", async () => {
    const res = await call("simulate_neutron_shield", {
      document_id: id,
      extract: {
        shells: [{ part: "shield" }],
        source: { rate_n_per_s: "src_rate", energy_ev: 2.45e6 },
        detectors: [{ label: "operator", radius_mm: 1000 }],
        run: { histories_per_batch: 2000, batches: 4, seed: 3 },
      },
    });
    expect(res.isError, res.content[0].text).toBeFalsy();
    const out = JSON.parse(res.content[0].text);
    expect(out.extracted.layer_parts).toEqual([null, "shield", null]);
    expect(out.extracted.spec.layers[1]).toEqual({ material: "hdpe", thickness_mm: 150 });
    expect(out.detectors[0].label).toBe("operator");
    expect(out.detectors[0].dose_usv_per_h).toBeGreaterThan(0);
  });

  it("spec and document_id are mutually exclusive", async () => {
    const res = await call("simulate_neutron_shield", {
      document_id: id,
      spec: { layers: [], source: {}, detectors: [] },
    });
    expect(res.isError).toBe(true);
    expect(res.content[0].text).toContain("not both");
  });
});
