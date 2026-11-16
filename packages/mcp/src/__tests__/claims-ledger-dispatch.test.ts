/**
 * Claims ledger through the REAL server: createServer dispatch, the disk
 * session store, a server restart, real solver outputs (solve_thermal,
 * simulate_em with force claims, analyze_antenna, analyze_tolerance_stackup)
 * attached verbatim, and a document that also carries geometry and a
 * persisted clearance spec — so build_receipt's own re-measuring can't move
 * the design fingerprint under measured rows.
 *
 * Skips when the loaded kernel WASM predates any of the solver bindings
 * (CI builds it from source).
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine } from "@vcad/engine";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { createServer } from "../server.js";
import { documents } from "../tools/session.js";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Any = any;

const probe = await Engine.init();
const k = (probe as unknown as { kernel?: Record<string, unknown> }).kernel ?? {};
const wasmHasSolvers = ["thermalSolve", "emSimulate", "antennaAnalyze", "toleranceAnalyze"].every(
  (n) => typeof k[n] === "function",
);

async function connect(engine: Engine) {
  const server = await createServer(engine, { user: null });
  const [clientT, serverT] = InMemoryTransport.createLinkedPair();
  const client = new Client({ name: "ledger-dispatch", version: "0.0.0" }, { capabilities: {} });
  await Promise.all([client.connect(clientT), server.connect(serverT)]);
  return client;
}

async function call(client: Client, name: string, args: Record<string, unknown>): Promise<Any> {
  const res = (await client.callTool({ name, arguments: args })) as Any;
  if (res.isError) throw new Error(`${name}: ${res.content[0].text}`);
  return JSON.parse(res.content[0].text);
}

const twoBlocks = {
  version: "0.1",
  nodes: {
    "1": { id: 1, name: "can", op: { type: "Cube", size: { x: 10, y: 10, z: 10 } } },
    "2": { id: 2, name: "pcb-raw", op: { type: "Cube", size: { x: 10, y: 10, z: 2 } } },
    "3": { id: 3, name: "pcb", op: { type: "Translate", child: 2, offset: { x: 15, y: 0, z: 0 } } },
  },
  materials: {},
  part_materials: {},
  roots: [
    { root: 1, material: "aluminum" },
    { root: 3, material: "fr4" },
  ],
};

const THERMAL = {
  spec: {
    origin_mm: [0, 0, 0],
    size_mm: [40, 40, 1.6],
    divisions: [20, 20, 2],
    materials: [{ shape: { type: "Box", min_mm: [0, 0, 0], size_mm: [40, 40, 1.6] }, k_w_mk: [15, 15, 0.5] }],
    sources: [{ name: "die", shape: { type: "Box", min_mm: [15, 15, 0], size_mm: [10, 10, 1.6] }, power_w: 1.0 }],
    domain_faces: [
      { type: "Adiabatic" },
      { type: "Adiabatic" },
      { type: "Adiabatic" },
      { type: "Adiabatic" },
      { type: "Convection", h_w_m2k: 12, ambient_c: 25 },
      { type: "Convection", h_w_m2k: 12, ambient_c: 25 },
    ],
  },
};

const EM = {
  spec: {
    problem: "axisym_magnetostatics",
    r_max_mm: 40,
    z_min_mm: 0,
    z_max_mm: 100,
    bc_r_outer: "neumann",
    bc_z_low: "neumann",
    bc_z_high: "neumann",
    coils: [{ region: { x_min_mm: 20, x_max_mm: 22, y_min_mm: 0, y_max_mm: 100 }, turns: 1000, current_a: 1.0 }],
  },
  options: { nx: 41, ny: 21, force_coil: 0 },
};

const ANTENNA = {
  spec: {
    elements: [{ type: "wire", start_mm: [0, 0, -500], end_mm: [0, 0, 500], radius_mm: 1.0, segments: 20 }],
    feed_mm: [0, 0, 0],
  },
  band: { f_lo_hz: 120e6, f_hi_hz: 165e6, points: 10 },
};

const TOLERANCE = {
  spec: {
    name: "can-to-lid",
    contributors: [
      { name: "can", coeff: 1.0, nominal: 20.0, tol_minus: 0.15, tol_plus: 0.15, dist: { type: "normal_from_tol", convention: { type: "three_sigma" } } },
      { name: "lid", coeff: -1.0, nominal: 19.5, tol_minus: 0.05, tol_plus: 0.05, dist: { type: "normal_from_tol", convention: { type: "three_sigma" } } },
    ],
    requirement: { name: "gap", lower_mm: 0.2, upper_mm: 0.8 },
  },
  options: { n: 2000, seed: 1, batches: 4 },
};

/** Series-RLC resonance at f0 across the antenna claim band, as a 50 Ω .s1p. */
function s1p(f0: number): string {
  const R = 70, L = 1e-6, C = 1 / ((2 * Math.PI * f0) ** 2 * L);
  const out = ["# HZ S RI R 50"];
  for (let i = 0; i <= 90; i++) {
    const f = 120e6 + (i * 45e6) / 90;
    const w = 2 * Math.PI * f;
    const x = w * L - 1 / (w * C);
    const a = R - 50, b = R + 50, d = b * b + x * x;
    out.push(`${f} ${(a * b + x * x) / d} ${(x * b - a * x) / d}`);
  }
  return out.join("\n");
}

describe.skipIf(!wasmHasSolvers)("claims ledger through the real server", () => {
  let engine: Engine;
  beforeAll(async () => {
    engine = await Engine.init();
    process.env.VCAD_MCP_SESSION_DIR = mkdtempSync(join(tmpdir(), "vcad-ledger-"));
  });
  afterAll(() => {
    delete process.env.VCAD_MCP_SESSION_DIR;
  });

  it("attaches real solver outputs, survives a restart, measures, and scores", async () => {
    documents.clear();
    let client = await connect(engine);
    const docId = (await call(client, "open_document", { initial: twoBlocks })).document_id as string;
    await call(client, "check_clearance", { document_id: docId, group_a: ["can"], group_b: ["pcb"], min_mm: 2, label: "can-to-pcb" });

    // Real solver outputs, attached verbatim (the whole tool result).
    const results: Record<string, Any> = {};
    for (const [tool, args] of [
      ["solve_thermal", THERMAL],
      ["simulate_em", EM],
      ["analyze_antenna", ANTENNA],
      ["analyze_tolerance_stackup", TOLERANCE],
    ] as const) {
      const r = await client.callTool({ name: tool, arguments: args as Record<string, unknown> });
      expect((r as Any).isError, tool).toBeFalsy();
      results[tool] = r;
      const a = await call(client, "attach_claims", { document_id: docId, claims: r, source_tool: tool });
      expect(a.attached.length, tool).toBeGreaterThan(0);
    }
    const before = await call(client, "build_receipt", { document_id: docId });
    const domains = new Set(before.unified.claims.map((c: Any) => c.domain));
    for (const d of ["mechanical", "thermal", "em", "antenna", "tolerance"]) expect(domains.has(d), d).toBe(true);
    expect(before.unified_summary.verdict).not.toBe("pass");
    expect(before.scorecard.counts.holds).toBe(0);

    // Restart: a fresh server process reads the session back from disk.
    documents.clear();
    client = await connect(engine);

    const ledgerIds = before.scorecard.rows.map((r: Any) => r.claim);
    const tId = ledgerIds.find((id: string) => id === "thermal.t_max_c");
    expect(tId).toBeDefined();
    const tPred = before.scorecard.rows.find((r: Any) => r.claim === tId).predicted.value as number;
    // In band (×/÷1.25).
    await call(client, "record_measurement", {
      document_id: docId,
      claim_id: tId,
      value: tPred * 1.1,
      unit: "C",
      uncertainty: 0.5,
      instrument: "type-K",
      band: { band_factor: 1.25 },
    });
    // Out of band: L measured 3× the prediction, ×/÷1.2.
    const lRow = before.scorecard.rows.find((r: Any) => r.claim.startsWith("em.") && r.predicted.unit === "H");
    expect(lRow).toBeDefined();
    await call(client, "record_measurement", {
      document_id: docId,
      claim_id: lRow.claim,
      ...(lRow.subject ? { subject: lRow.subject } : {}),
      value: (lRow.predicted.value as number) * 3,
      unit: "H",
      uncertainty: 0,
      instrument: "LCR meter",
      band: { band_factor: 1.2 },
    });
    // NanoVNA sweep resonant near the predicted resonance.
    const resRow = before.scorecard.rows.find((r: Any) => r.claim === "antenna.resonant_frequency");
    expect(resRow).toBeDefined();
    {
      const imp = await call(client, "import_measurement", {
        document_id: docId,
        format: "touchstone_s1p",
        text: s1p((resRow.predicted.value as number) * 1.005),
      });
      expect(imp.recorded).toContain("antenna.resonant_frequency");
    }

    const after = await call(client, "build_receipt", { document_id: docId });
    const row = (id: string) => after.scorecard.rows.find((r: Any) => r.claim === id);
    expect(row(tId).row).toBe("holds");
    expect(row(lRow.claim).row).toBe("violated");
    expect(row("antenna.resonant_frequency").row).toBe("holds");
    expect(after.scorecard.counts.stale).toBe(0);
    expect(after.unified_summary.verdict).toBe("fail");
    // The clearance claim is still in the same receipt.
    expect(after.unified.claims.some((c: Any) => c.id.startsWith("mech.clearance"))).toBe(true);

    // Verify through dispatch after one more restart: same rows, nothing moved.
    documents.clear();
    client = await connect(engine);
    const v = await call(client, "verify_receipt", { document_id: docId, receipt: after });
    expect(v.claims_ledger.status).toBe("Violated");
    expect(v.scorecard.counts.stale).toBe(0);
    expect(v.scorecard.rows.map((r: Any) => r.claim)).toEqual(
      after.unified.claims.filter((c: Any) => after.scorecard.rows.some((r: Any) => r.claim === c.id)).map((c: Any) => c.id),
    );
  });
});
