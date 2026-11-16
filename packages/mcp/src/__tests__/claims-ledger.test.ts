/**
 * Claims ledger end to end: several solver domains' predictions frozen on one
 * document, bench measurements bound (by hand and through the typed
 * importers), one receipt + scorecard, and the freeze guarantee (an edited
 * prediction or design reads Stale, never silently re-predicted).
 *
 * The solver results and instrument files are SYNTHETIC fixtures shaped like
 * the real outputs (`receipt_claims` + `claim_set`, as `vcad-kernel-wasm`
 * returns them; Touchstone / REW / CSV as the instruments export them).
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeEach, describe, expect, it } from "vitest";
import type { Document } from "@vcad/ir";
import { buildReceipt, verifyReceipt } from "../tools/ecad.js";
import { documents, getSession, openDocument } from "../tools/session.js";
import {
  attachClaims,
  bandHolds,
  designFingerprint,
  measurementsFromS1p,
  parseS1p,
  temperatureFromThermocoupleCsv,
  toolDefs,
  tuningFromRewImpedance,
} from "../tools/claims-ledger.js";
import { toolDefs as printToolDefs } from "../tools/print-check.js";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Any = any;

function out(result: { content: Array<{ type: string; text: string }>; isError?: boolean }): Any {
  if (result.isError) throw new Error(result.content[0]!.text);
  return JSON.parse(result.content[0]!.text);
}

const tool = (name: string) => {
  const t = [...toolDefs, ...printToolDefs].find((d) => d.name === name);
  if (!t) throw new Error(`no tool ${name}`);
  return (args: Record<string, unknown>) => t.handler(args, {} as Any) as Promise<Any>;
};
const attach = tool("attach_claims");
const record = tool("record_measurement");
const importMeasurement = tool("import_measurement");

// ------------------------------------------------------------- fixtures

const claim = (id: string, domain: string, value: number, unit: string, oracle: string, details = "solver provenance") => ({
  id,
  domain,
  description: `${id} prediction`,
  oracle: { id: oracle, version: "0.11.0" },
  verdict: "provisional",
  basis: "predicted",
  measured: unit === "1" ? { value } : { value, unit },
  details,
});

const thermalResult = {
  t_max_c: 58.4,
  claim_set: {
    schema: "vcad.thermal-claims/1",
    provenance: { grid: [20, 20, 10] },
    claims: [
      { name: "t_max_c", value: 58.4, unit: "C", basis: "predicted", note: "" },
      { name: "theta_ja_c_per_w", value: 11.2, unit: "K/W", basis: "predicted", note: "" },
    ],
  },
  receipt_claims: [
    claim("thermal.t_max_c", "thermal", 58.4, "C", "vcad-kernel-thermal/solve"),
    claim("thermal.theta_ja_c_per_w", "thermal", 11.2, "K/W", "vcad-kernel-thermal/solve"),
  ],
};

const antennaSet = {
  schema: "vcad.antenna-claims/1",
  provenance: { band: { f_lo_hz: 2.3e9, f_hi_hz: 2.6e9, points: 31 }, segments: 41 },
  reference_ohm: 50,
  claims: [
    { name: "resonant_frequency", value: 2.44e9, unit: "Hz", basis: "predicted", note: "" },
    { name: "z_in_re", value: 50, unit: "ohm", basis: "predicted", note: "" },
    { name: "gain_dbi", value: 2.1, unit: "dBi", basis: "predicted", note: "" },
  ],
};
const antennaResult = {
  claim_set: antennaSet,
  receipt_claims: antennaSet.claims.map((c) =>
    claim(`antenna.${c.name}`, "antenna", c.value, c.unit, "vcad-kernel-antenna/mom"),
  ),
};

const emResult = {
  claim_sets: [
    {
      schema: "vcad.em-claims/1",
      provenance: {},
      claims: [{ name: "self_inductance_h", value: 1.2e-3, unit: "H", basis: "predicted", note: "" }],
    },
  ],
  receipt_claims: [claim("em.self_inductance_h", "em", 1.2e-3, "H", "vcad-kernel-em/fv")],
};

const toleranceResult = {
  claim_set: {
    schema: "vcad.tolerance-claims/1",
    claims: [{ name: "gap_rss_mm", value: 0.12, uncertainty: 0.01, unit: "mm", basis: "predicted", note: "" }],
  },
  receipt_claims: [claim("tolerance.gap_rss_mm", "tolerance", 0.12, "mm", "vcad-kernel-tolerance/analysis")],
};

const acousticsResult = {
  claim_set: { schema: "vcad.acoustics-claims/1", claims: [{ name: "tuning_hz", value: 38, unit: "Hz" }] },
  receipt_claims: [claim("acoustics.tuning_hz", "acoustics", 38, "Hz", "vcad-kernel-acoustics/helmholtz")],
};

/** Series-RLC antenna resonant at `f0`, swept 2.3–2.6 GHz, as a 50 Ω RI .s1p. */
function s1pFixture(f0: number): string {
  const R = 48;
  const L = 8e-9;
  const C = 1 / ((2 * Math.PI * f0) ** 2 * L);
  const lines = ["! synthetic NanoVNA export", "# HZ S RI R 50"];
  for (let i = 0; i <= 60; i++) {
    const f = 2.3e9 + (i * 0.3e9) / 60;
    const w = 2 * Math.PI * f;
    const x = w * L - 1 / (w * C);
    // S11 = (Z - 50) / (Z + 50), Z = R + jX
    const nr = R - 50, nr2 = R + 50;
    const d = nr2 * nr2 + x * x;
    lines.push(`${f} ${(nr * nr2 + x * x) / d} ${(x * nr2 - nr * x) / d}`);
  }
  return lines.join("\n");
}

/** Vented-box impedance: peaks near 25 and 80 Hz, saddle at 45 Hz. */
function rewFixture(): string {
  const lines = ["* REW impedance export (synthetic)", "* Freq(Hz) Magnitude(ohm) Phase(deg)"];
  for (let f = 10; f <= 200; f += 1) {
    const peak = (fc: number, h: number, w: number) => h / (1 + ((f - fc) / w) ** 2);
    const z = 6 + peak(25, 30, 5) + peak(80, 22, 10) - (f === 45 ? 0.5 : 0);
    lines.push(`${f.toFixed(3)} ${z.toFixed(4)} 0.0`);
  }
  return lines.join("\n");
}

/** Type-K log settling to 62 °C. */
function thermocoupleFixture(): string {
  const lines = ["time_s,tc1_c"];
  for (let t = 0; t <= 1800; t += 10) lines.push(`${t},${(22 + 40 * (1 - Math.exp(-t / 200))).toFixed(3)}`);
  return lines.join("\n");
}

function emptyDoc(): Document {
  return { version: "0.1", nodes: {}, materials: {}, part_materials: {}, roots: [] } as unknown as Document;
}

beforeEach(() => documents.clear());

// ------------------------------------------------------------------ tests

describe("band math", () => {
  it("matches the shared Rust vectors (vcad-receipt/tests/ledger_band_vectors.json)", () => {
    const here = dirname(fileURLToPath(import.meta.url));
    const raw = readFileSync(join(here, "../../../../crates/vcad-receipt/tests/ledger_band_vectors.json"), "utf8");
    const { cases } = JSON.parse(raw) as { cases: Any[] };
    expect(cases.length).toBeGreaterThanOrEqual(8);
    for (const c of cases) {
      expect(bandHolds(c.band, c.predicted, c.measured, c.uncertainty, c.prediction_uncertainty ?? 0), c.name).toBe(c.holds);
    }
  });
});

describe("typed importers", () => {
  it("parses a Touchstone sweep and finds the resonance inside the claimed band", () => {
    const ms = measurementsFromS1p(parseS1p(s1pFixture(2.45e9)), antennaSet as Any);
    const res = ms.find((m) => m.claim === "resonant_frequency")!;
    expect(res.value).toBeGreaterThan(2.44e9);
    expect(res.value).toBeLessThan(2.46e9);
    expect(ms.find((m) => m.claim === "resonance_in_band")!.value).toBe(1);
  });

  it("refuses malformed instrument files instead of guessing", () => {
    expect(() => parseS1p("1 2 3")).toThrow(/before the # option line/);
    expect(() => parseS1p("# HZ Z RI R 50\n1 2 3")).toThrow(/S-parameter/);
    expect(() => tuningFromRewImpedance("10 5 0\n11 6 0\n12 7 0\n13 8 0\n14 9 0")).toThrow(/peak/);
    expect(() => temperatureFromThermocoupleCsv("a,b\n1,2", {})).toThrow();
    // Still heating: not steady state.
    const ramp = ["time_s,temp_c", ...Array.from({ length: 50 }, (_, i) => `${i * 10},${20 + i}`)].join("\n");
    expect(() => temperatureFromThermocoupleCsv(ramp, {})).toThrow(/steady state/);
  });

  it("reads the REW saddle and the thermocouple plateau", () => {
    expect(tuningFromRewImpedance(rewFixture()).f_b_hz).toBe(45);
    const t = temperatureFromThermocoupleCsv(thermocoupleFixture(), {});
    expect(t.value_c).toBeGreaterThan(61.8);
    expect(t.value_c).toBeLessThan(62.01);
  });
});

describe("claims ledger end to end", () => {
  it("one receipt carries five domains; measurements score Holds / Violated / Unmeasured; edits read Stale", async () => {
    const docId = out(openDocument({ initial: emptyDoc() })).document_id as string;

    // 1. Freeze predictions from five solvers (one via build_receipt's input).
    const a1 = out(await attach({ document_id: docId, claims: thermalResult, source_tool: "solve_thermal" }));
    expect(a1.attached).toEqual(["thermal.t_max_c", "thermal.theta_ja_c_per_w"]);
    out(await attach({ document_id: docId, claims: [antennaResult, emResult], source_tool: "analyze_antenna" }));
    out(await attach({ document_id: docId, claims: toleranceResult, source_tool: "analyze_tolerance_stackup" }));
    const first = out(
      await buildReceipt({ document_id: docId, attach_claims: acousticsResult, attach_source_tool: "acoustics" }),
    );
    expect(first.attached.attached).toEqual(["acoustics.tuning_hz"]);

    // Predicted only: every row Provisional, the receipt never Pass.
    expect(first.unified.schema).toBe("vcad.receipt/1");
    expect(first.unified.claims.map((c: Any) => c.id)).toEqual([
      "thermal.t_max_c",
      "thermal.theta_ja_c_per_w",
      "antenna.resonant_frequency",
      "antenna.z_in_re",
      "antenna.gain_dbi",
      "em.self_inductance_h",
      "tolerance.gap_rss_mm",
      "acoustics.tuning_hz",
    ]);
    expect(first.unified_summary.verdict).toBe("provisional");
    expect(first.unified.claims.every((c: Any) => c.verdict === "provisional" && c.basis === "predicted")).toBe(true);
    // Oracle identity and solver provenance survive.
    expect(first.unified.claims[2].oracle).toEqual({ id: "vcad-kernel-antenna/mom", version: "0.11.0" });
    expect(JSON.parse(first.unified.claims[0].details).solver_details).toBe("solver provenance");
    expect(first.unified.document_fingerprint).toMatch(/^[0-9a-f]{64}$/);
    expect(first.scorecard.counts).toEqual({ holds: 0, violated: 0, unmeasured: 8, stale: 0 });

    // 2. Measurements.
    // Thermocouple: 62 °C against 58.4 predicted, ×/÷1.25 → Holds.
    const tc = out(
      await importMeasurement({
        document_id: docId,
        format: "thermocouple_csv",
        text: thermocoupleFixture(),
        file_name: "tc.csv",
        claim_id: "thermal.t_max_c",
        band: { band_factor: 1.25 },
        instrument_uncertainty_c: 1.1,
      }),
    );
    expect(tc.raw_sha256).toMatch(/^[0-9a-f]{64}$/);
    // NanoVNA: resonance 2.45 GHz vs 2.44 predicted, ±2% → Holds; z_in_re also bound.
    const vna = out(
      await importMeasurement({ document_id: docId, format: "touchstone_s1p", text: s1pFixture(2.45e9) }),
    );
    expect(vna.recorded).toContain("antenna.resonant_frequency");
    expect(vna.derived.skipped_not_attached).toContain("s11_db_at_band");
    // REW: f_b 45 Hz vs 38 predicted, ×/÷1.1 → Violated.
    out(
      await importMeasurement({
        document_id: docId,
        format: "rew_impedance",
        text: rewFixture(),
        band: { band_factor: 1.1 },
      }),
    );
    // Generic hand entry: tolerance gap 0.15 mm, ±0.02 (+u 0.01 +pred u 0.01) → Holds.
    const hand = out(
      await record({
        document_id: docId,
        claim_id: "tolerance.gap_rss_mm",
        value: 0.15,
        unit: "mm",
        uncertainty: 0.01,
        instrument: "feeler gauge set",
        band: { half_width: 0.02 },
        raw_sha256: "ab".repeat(32),
      }),
    );
    expect(hand.recorded).toEqual(["tolerance.gap_rss_mm"]);

    // A measurement naming no claim is an error, and records nothing.
    const bogus = await record({
      document_id: docId,
      claim_id: "thermal.nope",
      value: 1,
      uncertainty: 0,
      instrument: "x",
      band: { band_factor: 1.1 },
    });
    expect(bogus.isError).toBe(true);
    expect(bogus.content[0].text).toMatch(/names no claim/);
    // Wrong band shape for the domain, wrong unit, missing band: all errors.
    for (const bad of [
      { claim_id: "em.self_inductance_h", value: 1e-3, uncertainty: 0, instrument: "LCR", band: { half_width: 1e-4 } },
      { claim_id: "em.self_inductance_h", value: 1, unit: "mH", uncertainty: 0, instrument: "LCR", band: { band_factor: 1.2 } },
      { claim_id: "em.self_inductance_h", value: 1e-3, uncertainty: 0, instrument: "LCR" },
    ]) {
      expect((await record({ document_id: docId, ...bad })).isError).toBe(true);
    }

    // 3. Receipt + scorecard.
    const built = out(await buildReceipt({ document_id: docId }));
    const byId = Object.fromEntries(built.unified.claims.map((c: Any) => [c.id, c]));
    expect(byId["thermal.t_max_c"].verdict).toBe("pass");
    expect(byId["thermal.t_max_c"].basis).toBe("measured");
    expect(byId["thermal.t_max_c"].predicted).toEqual({ value: 58.4, unit: "C" });
    expect(byId["antenna.resonant_frequency"].verdict).toBe("pass");
    expect(byId["acoustics.tuning_hz"].verdict).toBe("fail");
    expect(byId["tolerance.gap_rss_mm"].verdict).toBe("pass");
    expect(byId["em.self_inductance_h"].verdict).toBe("provisional");
    expect(built.unified_summary.verdict).toBe("fail");

    const verified = out(await verifyReceipt({ document_id: docId, receipt: built }));
    expect(verified.status).toBe("Violated");
    const rows = verified.scorecard.rows;
    expect(rows.map((r: Any) => r.claim)).toEqual(built.unified.claims.map((c: Any) => c.id));
    const row = (id: string) => rows.find((r: Any) => r.claim === id).row;
    expect(row("thermal.t_max_c")).toBe("holds");
    expect(row("antenna.resonant_frequency")).toBe("holds");
    expect(row("acoustics.tuning_hz")).toBe("violated");
    expect(row("tolerance.gap_rss_mm")).toBe("holds");
    expect(row("em.self_inductance_h")).toBe("unmeasured");
    expect(row("antenna.gain_dbi")).toBe("unmeasured");
    expect(verified.scorecard.markdown).toContain("| # | claim | domain | predicted | measured | band | verdict | instrument |");
    expect(verified.scorecard.markdown).toContain("❌ Violated");

    // Recording a measurement is not a design change: the fingerprint holds.
    const doc = getSession(docId);
    const fp = await designFingerprint(doc);
    expect(fp).toBe(built.unified.document_fingerprint);

    // 4. Freeze: re-attaching a different prediction is refused…
    const moved = structuredClone(thermalResult);
    moved.receipt_claims[0]!.measured = { value: 61.9, unit: "C" };
    const refused = await attach({ document_id: docId, claims: moved });
    expect(refused.isError).toBe(true);
    expect(refused.content[0].text).toMatch(/frozen/);
    // …and a hand edit of the frozen number reads Stale, not re-predicted.
    const entry = doc.claims_ledger!.entries.find((e) => e.claim.id === "thermal.t_max_c")!;
    entry.predicted = { value: 61.9, unit: "C" };
    const edited = out(await verifyReceipt({ document_id: docId, receipt: built }));
    const tRow = edited.scorecard.rows.find((r: Any) => r.claim === "thermal.t_max_c");
    expect(tRow.row).toBe("stale");
    expect(tRow.stale_reason).toBe("prediction_changed");
    entry.predicted = { value: 58.4, unit: "C" };
    expect(out(await verifyReceipt({ document_id: docId, receipt: built })).scorecard.rows[0].row).toBe("holds");

    // supersede replaces the prediction but keeps the measurement → Stale.
    out(await attach({ document_id: docId, claims: moved, supersede: true }));
    const superseded = out(await buildReceipt({ document_id: docId }));
    const sc = superseded.unified.claims.find((c: Any) => c.id === "thermal.t_max_c");
    expect(sc.verdict).toBe("unverifiable");
    expect(JSON.parse(sc.details).stale_reason).toBe("prediction_changed");

    // A design edit after measurement: every measured row reads Stale.
    (doc as Any).parameters = { wall_mm: { value: 3 } };
    const afterEdit = out(await verifyReceipt({ document_id: docId, receipt: built }));
    // Even the Violated row goes Stale: a moved design proves nothing either way.
    expect(afterEdit.status).toBe("Stale");
    expect(afterEdit.scorecard.rows.find((r: Any) => r.claim === "antenna.resonant_frequency").row).toBe("stale");
    expect(afterEdit.scorecard.rows.find((r: Any) => r.claim === "antenna.resonant_frequency").stale_reason).toBe(
      "design_changed",
    );
    expect(afterEdit.scorecard.counts.violated).toBe(0);
  });

  it("ledger-only receipts verify Holds when nothing moved, and Stale when a row changed since", async () => {
    const docId = out(openDocument({ initial: emptyDoc() })).document_id as string;
    out(await attach({ document_id: docId, claims: emResult }));
    const built = out(await buildReceipt({ document_id: docId }));
    const v1 = out(await verifyReceipt({ document_id: docId, receipt: built }));
    expect(v1.status).toBe("Holds");
    expect(v1.scorecard.counts.unmeasured).toBe(1);
    out(
      await record({
        document_id: docId,
        claim_id: "em.self_inductance_h",
        value: 1.25e-3,
        unit: "H",
        uncertainty: 1e-5,
        instrument: "LCR meter @1 kHz",
        band: { band_factor: 1.1 },
      }),
    );
    // Unmeasured → Holds since the receipt: rebuild to certify it.
    const v2 = out(await verifyReceipt({ document_id: docId, receipt: built }));
    expect(v2.status).toBe("Stale");
    expect(v2.scorecard.rows[0].row).toBe("holds");
    expect(v2.scorecard.rows[0].receipt_row).toBe("unmeasured");
    const rebuilt = out(await buildReceipt({ document_id: docId }));
    expect(rebuilt.unified_summary.verdict).toBe("pass");
    expect(out(await verifyReceipt({ document_id: docId, receipt: rebuilt })).status).toBe("Holds");
  });

  it("refuses duplicate claims in one batch and namespaces repeat runs with a label", async () => {
    const doc = emptyDoc();
    await expect(attachClaims(doc, [thermalResult, thermalResult])).rejects.toThrow(/twice/);
    await attachClaims(doc, thermalResult, { label: "idle" });
    await attachClaims(doc, thermalResult, { label: "full_volume" });
    expect(doc.claims_ledger!.entries).toHaveLength(4);
    // Re-attaching the identical prediction is a no-op.
    const again = await attachClaims(doc, thermalResult, { label: "idle" });
    expect(again.unchanged).toHaveLength(2);
    // A JSON round trip (session save/reload) keeps the ledger intact.
    const back = JSON.parse(JSON.stringify(doc)) as Document;
    expect(back.claims_ledger).toEqual(doc.claims_ledger);
  });
});
