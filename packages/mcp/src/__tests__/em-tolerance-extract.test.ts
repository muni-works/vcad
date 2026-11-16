import { describe, it, expect, beforeAll } from "vitest";
import { Engine } from "@vcad/engine";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { createServer } from "../server.js";
import { documents } from "../tools/session.js";
import { getSession } from "../tools/session-core.js";

/**
 * CAD → EM and CAD → tolerance over MCP: `simulate_em` and
 * `analyze_tolerance_stackup` taking a `document_id` + selection instead of
 * a hand-written spec (`vcad_eval::sim_extract::{em, tolerance}`).
 *
 * Skips when the loaded kernel WASM predates the extraction bindings (the
 * checked-in artifacts refresh only on main; CI builds from source).
 */

const probeEngine = await Engine.init();
const wasmHasExtract =
  typeof (
    probeEngine as unknown as {
      kernel?: { extractAxisymMagSpec?: unknown; extractStackupSpec?: unknown };
    }
  ).kernel?.extractAxisymMagSpec === "function";

// 3" speaker motor: T-yoke, Y30 ring, top plate, 0.5 mm gap, voice coil.
const MOTOR =
  "[defparam vc_turns 60.0]\n" +
  '[root-named [union [cylinder 30 4] [translate 0 0 1 [cylinder 9.75 15]]] "steel-1010" "yoke"]\n' +
  '[root-named [translate 0 0 4 [difference [translate 0 0 -1 [cylinder 12 10]] [cylinder 30 8]]] "ferrite-y30" "magnet"]\n' +
  '[root-named [translate 0 0 12 [difference [translate 0 0 -1 [cylinder 10.25 6]] [cylinder 30 4]]] "steel-1010" "top_plate"]\n' +
  '[root-named [translate 0 0 10 [difference [translate 0 0 -1 [cylinder 9.85 10]] [cylinder 10.15 8]]] "copper" "voice_coil"]';

// Lid slip-fit into a rim; both diameters FDM ±0.1.
const FIT =
  '[defparam rim_id 60.3 :tol 0.1]\n[defparam lid_od 60.0 :tol 0.1]\n' +
  '[root-named [difference [translate 0 0 14 [cylinder [* 0.5 rim_id] 7]] [cylinder 35 20]] "pla" "rim"]\n' +
  '[root-named [union [translate 0 0 14 [cylinder [* 0.5 lid_od] 6.5]] [translate 0 0 20 [cylinder 35 2]]] "pla" "lid"]';

type ToolOut = { isError?: boolean; content: Array<{ type: string; text: string }> };

describe.skipIf(!wasmHasExtract)("document → EM / tolerance extraction tools", () => {
  let call: (name: string, args: Record<string, unknown>) => Promise<ToolOut>;
  let motorId: string;
  let fitId: string;

  beforeAll(async () => {
    documents.clear();
    const engine = await Engine.init();
    const server = await createServer(engine, { user: null });
    const [clientT, serverT] = InMemoryTransport.createLinkedPair();
    const client = new Client({ name: "em-tol-extract", version: "0.0.0" }, { capabilities: {} });
    await Promise.all([client.connect(clientT), server.connect(serverT)]);
    call = async (name, args) => (await client.callTool({ name, arguments: args })) as ToolOut;
    const open = async (source: string) => {
      const id = JSON.parse((await call("open_document", {})).content[0].text).document_id;
      const created = await call("create_cad_loon", { document_id: id, mode: "add", source });
      expect(created.isError, created.content[0]?.text).toBeFalsy();
      return id as string;
    };
    motorId = await open(MOTOR);
    fitId = await open(FIT);
    const doc = getSession(fitId);
    doc.clearance_specs = [
      { label: "lid-fit", group_a: ["lid"], group_b: ["rim"], min_mm: 0 },
    ] as typeof doc.clearance_specs;
  });

  const motorExtract = {
    parts: [
      { part: "yoke", linear: true },
      { part: "magnet" },
      { part: "top_plate", linear: true },
      { part: "voice_coil", role: "coil", turns: "vc_turns" },
    ],
    domain: { pad_factor: 0, r_max_mm: 60, z_min_mm: -30, z_max_mm: 46, cells_per_feature: 2 },
  };

  it("simulate_em prices gap flux and Bl from the document's own motor", async () => {
    const res = await call("simulate_em", {
      document_id: motorId,
      extract: motorExtract,
    });
    expect(res.isError, res.content[0].text).toBeFalsy();
    const out = JSON.parse(res.content[0].text);
    expect(out.extracted.grid.thinnest_feature_mm).toBeCloseTo(0.5, 9);
    expect(out.extracted.air_gap).toEqual([9.75, 10.25, 12, 16]);
    expect(out.extracted.spec.magnets).toHaveLength(1);
    expect(out.extracted.spec.coils[0].turns).toBe("vc_turns");
    const names = out.claim_sets[0].claims.map((c: { name: string }) => c.name);
    expect(names).toEqual(["gap_flux_t", "bl_n_per_a"]);
    expect(out.qois.gap_flux_t).toBeGreaterThan(0.5);
    expect(out.qois.gap_flux_t).toBeLessThan(2.3);
    expect(out.qois.self_inductance_h).toBeNull();
  });

  it("a grid coarser than the advice is refused", async () => {
    const res = await call("simulate_em", {
      document_id: motorId,
      extract: motorExtract,
      options: { nx: 81, ny: 81 },
    });
    expect(res.isError).toBe(true);
    expect(res.content[0].text).toContain("coarser than the advised");
  });

  it("a part with no magnetic role fails closed by name", async () => {
    const bad = await call("simulate_em", {
      document_id: motorId,
      extract: { parts: [{ part: "voice_coil" }] },
    });
    expect(bad.isError).toBe(true);
    expect(bad.content[0].text).toContain("voice_coil");
    expect(bad.content[0].text).toContain("not in the magnetic material table");
  });

  it("analyze_tolerance_stackup derives the lid fit from the document", async () => {
    const res = await call("analyze_tolerance_stackup", {
      document_id: fitId,
      extract: { clearance: "lid-fit", feature: "radial", upper_mm: 0.3 },
      options: { n: 20000, seed: 5 },
    });
    expect(res.isError, res.content[0].text).toBeFalsy();
    const out = JSON.parse(res.content[0].text);
    expect(out.extracted.gap_mm).toBeCloseTo(0.15, 9);
    const coeffs = Object.fromEntries(
      out.extracted.contributors.map((c: { parameter: string; coeff: number }) => [
        c.parameter,
        c.coeff,
      ]),
    );
    expect(coeffs.rim_id).toBeCloseTo(0.5, 9);
    expect(coeffs.lid_od).toBeCloseTo(-0.5, 9);
  });
});
