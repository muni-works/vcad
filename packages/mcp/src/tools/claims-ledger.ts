/**
 * The claims ledger — one document carries every solver's frozen
 * predictions and, later, their bench measurements (`vcad.claims-ledger/1`,
 * Rust twin: `vcad_receipt::ledger`).
 *
 * The solver tools (`solve_thermal`, `analyze_antenna`, `simulate_em`,
 * `analyze_structure`, `analyze_tolerance_stackup`, `simulate_flow`, …) are stateless: each
 * returns its native claim set plus unified receipt claims. `attach_claims`
 * appends them to
 * `Document.claims_ledger`, freezing each prediction: the numeric value and
 * unit, a hash over the prediction, and the fingerprint of the design it was
 * priced on. `record_measurement { claim_id }` and `import_measurement`
 * (NanoVNA `.s1p`, REW impedance export, thermocouple CSV) bind bench
 * readings to entries, compared fail-closed with the domain's own band
 * shape. Rows read Holds / Violated / Unmeasured / Stale; `build_receipt`
 * rolls them into the unified receipt and `verify_receipt` re-checks them and
 * renders the scorecard.
 *
 * Freeze guarantee: once an entry has a measurement, a prediction whose hash
 * moved (re-attached with `supersede`, or edited by hand) or a design whose
 * fingerprint moved since the freeze reads **Stale** — never silently
 * re-predicted. The ledger itself is excluded from the document fingerprint,
 * so recording a measurement is not a design change.
 *
 * Hashes are computed here only (one canonical serialization, floats rounded
 * to 15 significant digits so a re-serialization by another runtime cannot
 * flip them); Rust compares them as opaque strings.
 */

import type {
  AcceptanceBand,
  ClaimQuantity,
  ClaimsLedger,
  Document,
  LedgerEntry,
  LedgerMeasurement,
  LedgerRow,
  ReceiptClaim,
  StaleReason,
} from "@vcad/ir";
import { resolveDocInput } from "./session-core.js";
import { behavior, type ToolDef } from "./tool-def.js";

export const LEDGER_SCHEMA = "vcad.claims-ledger/1";

type Json = Record<string, unknown>;

// ---------------------------------------------------------------- hashing

async function sha256Hex(data: string | Uint8Array): Promise<string> {
  const bytes = typeof data === "string" ? new TextEncoder().encode(data) : data;
  // WebCrypto, not node:crypto: this module stays loadable in browser bundles.
  const digest = await crypto.subtle.digest("SHA-256", bytes as BufferSource);
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}

/** Key-sorted JSON, so the hash never depends on property order. */
function canonicalJson(v: unknown): string {
  if (Array.isArray(v)) return `[${v.map(canonicalJson).join(",")}]`;
  if (v && typeof v === "object") {
    const o = v as Json;
    return `{${Object.keys(o)
      .filter((k) => o[k] !== undefined)
      .sort()
      .map((k) => `${JSON.stringify(k)}:${canonicalJson(o[k])}`)
      .join(",")}}`;
  }
  return JSON.stringify(v);
}

/** 15 significant digits: survives a last-ULP drift through another runtime. */
function canonNumber(x: unknown): unknown {
  return typeof x === "number" && Number.isFinite(x) ? Number(x.toPrecision(15)) : x;
}

function normUnit(u: string | null | undefined): string | null {
  const t = (u ?? "").trim();
  return t === "" || t === "1" ? null : t;
}

export function sameUnit(a: string | null | undefined, b: string | null | undefined): boolean {
  return normUnit(a) === normUnit(b);
}

/**
 * Hash of an entry's frozen prediction as it is NOW. Covers the design
 * fingerprint the prediction was priced on, so re-freezing an unchanged
 * number against a moved design (supersede after an edit) still breaks the
 * measurement's binding — a reading of the old hardware never launders back
 * to Holds.
 */
export async function predictionHash(e: LedgerEntry): Promise<string> {
  return sha256Hex(
    canonicalJson({
      design: e.document_fingerprint ?? null,
      id: e.claim.id,
      domain: e.claim.domain,
      subject: e.claim.subject ?? null,
      oracle: { id: e.claim.oracle.id, version: e.claim.oracle.version },
      value: canonNumber(e.predicted.value),
      unit: normUnit(e.predicted.unit),
      uncertainty: canonNumber(e.prediction_uncertainty ?? null),
    }),
  );
}

/**
 * Hash of the design: the whole document minus `source` (provenance text a
 * save rewrites) and `claims_ledger` (recording a measurement is not a design
 * change). Shared with `build_receipt`'s document binding.
 */
export async function designFingerprint(doc: Document): Promise<string> {
  const {
    source: _source,
    claims_ledger: _ledger,
    ...design
  } = doc as Document & { source?: unknown };
  return sha256Hex(JSON.stringify(design));
}

// ------------------------------------------------------------- band math

/** Band shape a domain's own `compare` uses; undefined = no comparison. */
export function domainBandKind(domain: string): "factor" | "absolute" | undefined {
  if (["thermal", "em", "acoustics", "flow"].includes(domain)) return "factor";
  if (["antenna", "tolerance"].includes(domain)) return "absolute";
  return undefined;
}

export function bandInterval(
  band: AcceptanceBand,
  predicted: number,
  uncertainty: number,
  predictionUncertainty = 0,
): [number, number] {
  if (band.kind === "factor") {
    const a = predicted / band.band_factor;
    const b = predicted * band.band_factor;
    return [Math.min(a, b) - uncertainty, Math.max(a, b) + uncertainty];
  }
  const w = band.half_width + uncertainty + predictionUncertainty;
  return [predicted - w, predicted + w];
}

export function bandHolds(
  band: AcceptanceBand,
  predicted: number,
  measured: number,
  uncertainty: number,
  predictionUncertainty = 0,
): boolean {
  const [lo, hi] = bandInterval(band, predicted, uncertainty, predictionUncertainty);
  return Number.isFinite(measured) && lo <= measured && measured <= hi;
}

function validateBand(band: AcceptanceBand): void {
  if (band.kind === "factor") {
    if (!(Number.isFinite(band.band_factor) && band.band_factor >= 1)) {
      throw new Error(`band_factor must be finite and >= 1 (got ${band.band_factor})`);
    }
  } else if (band.kind === "absolute") {
    if (!(Number.isFinite(band.half_width) && band.half_width >= 0)) {
      throw new Error(`half_width must be finite and >= 0 (got ${band.half_width})`);
    }
  } else {
    throw new Error(`unknown band kind ${(band as { kind?: unknown }).kind}`);
  }
}

/**
 * Parse a caller's band for a domain. Accepts `{band_factor}`,
 * `{half_width}`, or `{kind, ...}`. The measurer owns the band's size; the
 * domain owns its shape — a factor band on an antenna claim is an error.
 */
export function parseBand(raw: unknown, domain: string, claimId: string): AcceptanceBand {
  if (!raw || typeof raw !== "object") {
    throw new Error(
      `measurement for ${claimId} needs a band: {band_factor} or {half_width} — the measurer owns the acceptance band, vcad does not invent one`,
    );
  }
  const r = raw as Json;
  let band: AcceptanceBand;
  if (r.kind === "factor" || (r.kind === undefined && r.band_factor !== undefined)) {
    band = { kind: "factor", band_factor: Number(r.band_factor) };
  } else if (r.kind === "absolute" || (r.kind === undefined && r.half_width !== undefined)) {
    band = { kind: "absolute", half_width: Number(r.half_width) };
  } else {
    throw new Error(`band for ${claimId} must carry band_factor or half_width`);
  }
  validateBand(band);
  const want = domainBandKind(domain);
  if (want && want !== band.kind) {
    throw new Error(
      `${claimId}: the ${domain} domain compares with a ${want} band (${
        want === "factor" ? "{band_factor}" : "{half_width}"
      }), not ${band.kind}`,
    );
  }
  return band;
}

// ---------------------------------------------------------------- ledger

function emptyLedger(): ClaimsLedger {
  return { schema: LEDGER_SCHEMA, entries: [] };
}

export function ledgerOf(doc: Document): ClaimsLedger | undefined {
  const l = doc.claims_ledger;
  return l && Array.isArray(l.entries) ? l : undefined;
}

function entryKey(id: string, subject: string | null | undefined): string {
  return `${id}|${subject ?? ""}`;
}

export function keyOfEntry(e: LedgerEntry): string {
  return entryKey(e.claim.id, e.claim.subject);
}

function isReceiptClaim(v: unknown): v is ReceiptClaim {
  if (!v || typeof v !== "object") return false;
  const c = v as Json;
  return (
    typeof c.id === "string" &&
    typeof c.domain === "string" &&
    typeof c.verdict === "string" &&
    !!c.oracle &&
    typeof (c.oracle as Json).id === "string"
  );
}

interface NativeSet {
  schema: string;
  obj: Json;
}

interface Extracted {
  claims: ReceiptClaim[];
  native: NativeSet[];
}

/**
 * Pull unified claims (and native claim sets) out of whatever a solver
 * returned: the raw result (`receipt_claims` + `claim_set` / `claim_sets`), an
 * MCP tool result (`content[0].text` JSON), an array of either, a bare claim
 * array, or a single claim.
 */
export function extractClaims(payload: unknown): Extracted {
  const out: Extracted = { claims: [], native: [] };
  const visit = (p: unknown, depth: number): void => {
    if (depth > 4 || p === null || p === undefined) return;
    if (typeof p === "string") {
      try {
        visit(JSON.parse(p), depth + 1);
      } catch {
        throw new Error("attach_claims: a string payload must be JSON");
      }
      return;
    }
    if (Array.isArray(p)) {
      for (const x of p) visit(x, depth + 1);
      return;
    }
    if (typeof p !== "object") return;
    const o = p as Json;
    if (isReceiptClaim(o)) {
      out.claims.push(o);
      return;
    }
    if (Array.isArray(o.content) && o.content.length > 0) {
      const first = o.content[0] as Json;
      if (typeof first?.text === "string") visit(first.text, depth + 1);
      return;
    }
    if (Array.isArray(o.receipt_claims)) {
      for (const c of o.receipt_claims) {
        if (!isReceiptClaim(c)) throw new Error("attach_claims: receipt_claims holds a non-claim");
        out.claims.push(c);
      }
      const sets = [o.claim_set, ...(Array.isArray(o.claim_sets) ? o.claim_sets : [])];
      for (const s of sets) {
        if (s && typeof s === "object" && typeof (s as Json).schema === "string") {
          out.native.push({ schema: (s as Json).schema as string, obj: s as Json });
        }
      }
      return;
    }
    // A unified receipt: { schema: "vcad.receipt/1", claims: [...] }.
    if (Array.isArray(o.claims) && o.claims.every(isReceiptClaim)) {
      out.claims.push(...(o.claims as ReceiptClaim[]));
      return;
    }
    if (o.unified && typeof o.unified === "object") {
      visit(o.unified, depth + 1);
      return;
    }
  };
  visit(payload, 0);
  return out;
}

/** Native claim name for a unified id: `thermal.t_max_c` → `t_max_c`. */
function nativeName(id: string): string {
  const dot = id.indexOf(".");
  return dot >= 0 ? id.slice(dot + 1) : id;
}

function nativeClaim(set: NativeSet, id: string): Json | undefined {
  const claims = set.obj.claims;
  if (!Array.isArray(claims)) return undefined;
  const name = nativeName(id);
  return claims.find((c) => (c as Json)?.name === name) as Json | undefined;
}

/** The solver's computed number: adapters ride it in `measured` (what the
 *  oracle computed); fall back to `predicted`. */
function frozenPrediction(c: ReceiptClaim): ClaimQuantity | undefined {
  for (const q of [c.measured, c.predicted]) {
    if (q && typeof q.value === "number" && Number.isFinite(q.value)) return q;
  }
  return c.measured ?? c.predicted ?? undefined;
}

export interface AttachOptions {
  source_tool?: string;
  /** Subject for claims that carry none — namespaces two runs of one solver. */
  label?: string;
  /** Replace an already-frozen prediction (a measured entry then reads Stale). */
  supersede?: boolean;
  now?: string;
}

export interface AttachResult {
  attached: string[];
  unchanged: string[];
  superseded: string[];
  not_measurable: string[];
  ledger_size: number;
}

/** Freeze a batch of solver claims onto the document, all-or-nothing. */
export async function attachClaims(
  doc: Document,
  payload: unknown,
  opts: AttachOptions = {},
): Promise<AttachResult> {
  const { claims, native } = extractClaims(payload);
  if (claims.length === 0) {
    throw new Error(
      "attach_claims: no receipt claims found — pass a solver result (with receipt_claims), a claim array, or a unified receipt",
    );
  }
  const ledger = ledgerOf(doc) ?? emptyLedger();
  const claimSets = [...(ledger.claim_sets ?? [])];
  const fingerprint = await designFingerprint(doc);
  const now = opts.now ?? new Date().toISOString();

  // Register native sets (dedupe by content).
  const nativeIndex = new Map<NativeSet, number>();
  for (const n of native) {
    const json = JSON.stringify(n.obj);
    let idx = claimSets.findIndex((s) => s.json === json);
    if (idx < 0) {
      claimSets.push({ schema: n.schema, json });
      idx = claimSets.length - 1;
    }
    nativeIndex.set(n, idx);
  }

  const batchKeys = new Set<string>();
  const staged: LedgerEntry[] = [];
  for (const raw of claims) {
    const claim: ReceiptClaim = structuredClone(raw);
    if (claim.subject === undefined && opts.label) claim.subject = opts.label;
    const key = entryKey(claim.id, claim.subject);
    if (batchKeys.has(key)) {
      throw new Error(
        `attach_claims: claim ${claim.id}${claim.subject ? ` (${claim.subject})` : ""} appears twice in one batch — attach the runs separately with distinct \`label\`s`,
      );
    }
    batchKeys.add(key);
    const predicted = frozenPrediction(claim) ?? { value: "none" };
    // Prefer the set whose schema names the claim's domain (two domains may
    // share a native claim name).
    const holders = native.filter((n) => nativeClaim(n, claim.id) !== undefined);
    const set = holders.find((n) => n.schema.includes(`.${claim.domain}-`)) ?? holders[0];
    const nat = set ? nativeClaim(set, claim.id) : undefined;
    const u = nat && typeof nat.uncertainty === "number" ? nat.uncertainty : undefined;
    const entry: LedgerEntry = {
      claim,
      predicted,
      ...(u !== undefined ? { prediction_uncertainty: u } : {}),
      prediction_sha256: "",
      document_fingerprint: fingerprint,
      ...(set ? { claim_set_schema: set.schema, claim_set: nativeIndex.get(set)! } : {}),
      ...(opts.source_tool ? { source_tool: opts.source_tool } : {}),
      attached_at: now,
    };
    entry.prediction_sha256 = await predictionHash(entry);
    staged.push(entry);
  }

  const result: AttachResult = {
    attached: [],
    unchanged: [],
    superseded: [],
    not_measurable: [],
    ledger_size: 0,
  };
  const entries = [...ledger.entries];
  const conflicts: string[] = [];
  for (const e of staged) {
    const key = keyOfEntry(e);
    const label = e.claim.subject ? `${e.claim.id} (${e.claim.subject})` : e.claim.id;
    if (typeof e.predicted.value !== "number") result.not_measurable.push(label);
    const i = entries.findIndex((x) => keyOfEntry(x) === key);
    if (i < 0) {
      entries.push(e);
      result.attached.push(label);
      continue;
    }
    const prev = entries[i]!;
    if (prev.prediction_sha256 === e.prediction_sha256 && prev.document_fingerprint === e.document_fingerprint) {
      result.unchanged.push(label);
      continue;
    }
    if (!opts.supersede) {
      conflicts.push(label);
      continue;
    }
    // Keep the measurement: it was compared against the old prediction, so
    // the row now reads Stale until someone re-measures.
    entries[i] = { ...e, ...(prev.measurement ? { measurement: prev.measurement } : {}) };
    result.superseded.push(label);
  }
  if (conflicts.length > 0) {
    throw new Error(
      `attach_claims: ${conflicts.length} prediction(s) already frozen with a different value or design: ${conflicts.join(", ")}. ` +
        "Predictions are frozen — pass supersede:true to replace them (measured rows then read Stale), or attach under a new `label`.",
    );
  }
  doc.claims_ledger = {
    schema: LEDGER_SCHEMA,
    entries,
    ...(claimSets.length ? { claim_sets: claimSets } : {}),
  };
  result.ledger_size = entries.length;
  return result;
}

// ------------------------------------------------------------ measurement

export interface MeasurementInput {
  claim_id: string;
  subject?: string;
  value: number;
  unit?: string;
  uncertainty: number;
  instrument: string;
  band: AcceptanceBand;
  raw_sha256?: string;
  raw_name?: string;
  importer?: string;
}

function findEntry(ledger: ClaimsLedger, claimId: string, subject: string | undefined): LedgerEntry {
  const hits = ledger.entries.filter(
    (e) => e.claim.id === claimId && (subject === undefined || (e.claim.subject ?? "") === subject),
  );
  if (hits.length === 0) {
    const known = ledger.entries.map((e) => e.claim.id).slice(0, 40).join(", ");
    throw new Error(
      `measurement names no claim: ${claimId}${subject ? ` (subject ${subject})` : ""} is not in the document's claims ledger (attached: ${known || "none"})`,
    );
  }
  if (hits.length > 1) {
    throw new Error(
      `${claimId} is ambiguous — it is attached under subjects ${hits.map((h) => JSON.stringify(h.claim.subject ?? "")).join(", ")}; pass \`subject\``,
    );
  }
  return hits[0]!;
}

/** Bind bench readings to frozen predictions, all-or-nothing. */
export async function recordClaimMeasurements(
  doc: Document,
  inputs: MeasurementInput[],
  opts: { replace?: boolean; now?: string } = {},
): Promise<{ recorded: string[] }> {
  const ledger = ledgerOf(doc);
  if (!ledger || ledger.entries.length === 0) {
    throw new Error(
      "measurement names no claim: this document has no claims ledger — attach solver claims first (attach_claims)",
    );
  }
  if (inputs.length === 0) throw new Error("no measurements given");
  const now = opts.now ?? new Date().toISOString();
  const staged: Array<[LedgerEntry, LedgerMeasurement]> = [];
  const seen = new Set<LedgerEntry>();
  for (const m of inputs) {
    const e = findEntry(ledger, m.claim_id, m.subject);
    if (seen.has(e)) throw new Error(`${m.claim_id} is measured twice in one call`);
    seen.add(e);
    if (typeof e.predicted.value !== "number") {
      throw new Error(`${m.claim_id}: the frozen prediction is not a number (${JSON.stringify(e.predicted.value)}) — nothing to band against`);
    }
    if (!Number.isFinite(m.value)) throw new Error(`${m.claim_id}: value must be a finite number`);
    if (!(Number.isFinite(m.uncertainty) && m.uncertainty >= 0)) {
      throw new Error(`${m.claim_id}: uncertainty must be a finite number >= 0`);
    }
    if (!m.instrument || !m.instrument.trim()) {
      throw new Error(`${m.claim_id}: instrument provenance is required (what measured it, how)`);
    }
    const unit = m.unit ?? e.predicted.unit ?? undefined;
    if (!sameUnit(unit, e.predicted.unit)) {
      throw new Error(
        `${m.claim_id}: unit ${JSON.stringify(m.unit)} does not match the prediction's ${JSON.stringify(e.predicted.unit ?? "1")} — convert before recording`,
      );
    }
    validateBand(m.band);
    const want = domainBandKind(e.claim.domain);
    if (want && want !== m.band.kind) {
      throw new Error(`${m.claim_id}: the ${e.claim.domain} domain compares with a ${want} band, not ${m.band.kind}`);
    }
    if (e.measurement && !opts.replace) {
      throw new Error(`${m.claim_id} already has a measurement — pass replace:true to overwrite it`);
    }
    // Bind to the stored hash: a hand-edited prediction can't be laundered by
    // measuring it — the row recomputes the hash and reads Stale.
    staged.push([
      e,
      {
        value: m.value,
        ...(unit ? { unit } : {}),
        uncertainty: m.uncertainty,
        instrument: m.instrument,
        band: m.band,
        ...(m.raw_sha256 ? { raw_sha256: m.raw_sha256 } : {}),
        ...(m.raw_name ? { raw_name: m.raw_name } : {}),
        ...(m.importer ? { importer: m.importer } : {}),
        recorded_at: now,
        prediction_sha256: e.prediction_sha256,
      },
    ]);
  }
  for (const [e, m] of staged) e.measurement = m;
  return { recorded: staged.map(([e]) => (e.claim.subject ? `${e.claim.id} (${e.claim.subject})` : e.claim.id)) };
}

// ------------------------------------------------------------------ rows

export interface RowState {
  key: string;
  row: LedgerRow;
  stale_reason?: StaleReason;
  /** Design moved since the freeze, on an unmeasured row (informational). */
  design_changed?: boolean;
  interval?: [number, number];
}

export async function classifyEntry(e: LedgerEntry, currentFingerprint: string | undefined): Promise<RowState> {
  const key = keyOfEntry(e);
  const designChanged =
    currentFingerprint !== undefined &&
    e.document_fingerprint !== undefined &&
    e.document_fingerprint !== null &&
    e.document_fingerprint !== currentFingerprint;
  const m = e.measurement;
  if (!m) return { key, row: "unmeasured", ...(designChanged ? { design_changed: true } : {}) };
  const now = await predictionHash(e);
  if (now !== e.prediction_sha256 || m.prediction_sha256 !== e.prediction_sha256) {
    return { key, row: "stale", stale_reason: "prediction_changed" };
  }
  if (designChanged) return { key, row: "stale", stale_reason: "design_changed" };
  const p = e.predicted.value;
  if (typeof p !== "number" || !Number.isFinite(p)) {
    return { key, row: "stale", stale_reason: "prediction_changed" };
  }
  const u0 = e.prediction_uncertainty ?? 0;
  const interval = bandInterval(m.band, p, m.uncertainty, u0);
  return {
    key,
    row: bandHolds(m.band, p, m.value, m.uncertainty, u0) ? "holds" : "violated",
    interval,
  };
}

/** Marker in a ledger claim's `details` JSON. */
const LEDGER_MARKER = "ledger";

/** The unified receipt claim for one row (mirrors `LedgerEntry::receipt_claim`). */
export function ledgerReceiptClaim(e: LedgerEntry, s: RowState): ReceiptClaim {
  const c: ReceiptClaim = structuredClone(e.claim);
  const m = e.measurement;
  const measuredQ = m ? { value: m.value, ...(m.unit ?? e.predicted.unit ? { unit: (m.unit ?? e.predicted.unit)! } : {}) } : undefined;
  if (s.row === "unmeasured") {
    // A predicted pass is never a pass. (An explicitly verified oracle result
    // — a DFM rule check — keeps its verdict.)
    if (c.verdict === "pass" && (c.basis === undefined || c.basis === "predicted")) {
      c.basis = "predicted";
      c.verdict = "provisional";
    }
  } else if (s.row === "holds" || s.row === "violated") {
    c.verdict = s.row === "holds" ? "pass" : "fail";
    c.basis = "measured";
    c.predicted = e.predicted;
    c.measured = measuredQ!;
  } else {
    c.verdict = "unverifiable";
    c.predicted = e.predicted;
    if (measuredQ) c.measured = measuredQ;
  }
  const details: Json = {
    [LEDGER_MARKER]: LEDGER_SCHEMA,
    key: s.key,
    row: s.row,
    ...(s.stale_reason ? { stale_reason: s.stale_reason } : {}),
    ...(s.design_changed ? { design_changed: true } : {}),
    prediction_sha256: e.prediction_sha256,
    ...(e.document_fingerprint ? { document_fingerprint: e.document_fingerprint } : {}),
    ...(e.source_tool ? { source_tool: e.source_tool } : {}),
    ...(e.claim_set_schema ? { claim_set_schema: e.claim_set_schema } : {}),
    ...(e.prediction_uncertainty !== undefined ? { prediction_uncertainty: e.prediction_uncertainty } : {}),
    ...(m
      ? {
          measurement: {
            uncertainty: m.uncertainty,
            instrument: m.instrument,
            band: m.band,
            ...(s.interval ? { interval: s.interval } : {}),
            ...(m.raw_sha256 ? { raw_sha256: m.raw_sha256 } : {}),
            ...(m.raw_name ? { raw_name: m.raw_name } : {}),
            ...(m.importer ? { importer: m.importer } : {}),
            ...(m.recorded_at ? { recorded_at: m.recorded_at } : {}),
          },
        }
      : {}),
    ...(s.row === "stale"
      ? {
          reason:
            s.stale_reason === "design_changed"
              ? "the design changed since this prediction was frozen — re-run the solver and attach with supersede, then re-measure"
              : "the prediction changed since it was measured — never silently re-predicted",
        }
      : {}),
    ...(e.claim.details ? { solver_details: e.claim.details } : {}),
  };
  c.details = JSON.stringify(details);
  return c;
}

export function ledgerDetails(c: ReceiptClaim): Json | undefined {
  if (!c.details || c.details[0] !== "{") return undefined;
  try {
    const d = JSON.parse(c.details) as Json;
    return d[LEDGER_MARKER] === LEDGER_SCHEMA ? d : undefined;
  } catch {
    return undefined;
  }
}

export function hasLedgerClaims(receipt: { claims: ReceiptClaim[] }): boolean {
  return receipt.claims.some((c) => ledgerDetails(c) !== undefined);
}

/** Every ledger entry as a receipt claim, in ledger order. */
export async function ledgerReceiptClaims(doc: Document): Promise<ReceiptClaim[]> {
  const ledger = ledgerOf(doc);
  if (!ledger) return [];
  const fp = await designFingerprint(doc);
  const out: ReceiptClaim[] = [];
  for (const e of ledger.entries) out.push(ledgerReceiptClaim(e, await classifyEntry(e, fp)));
  return out;
}

// ------------------------------------------------------------- scorecard

export interface ScorecardRow {
  n: number;
  claim: string;
  subject?: string;
  domain: string;
  oracle: string;
  predicted: ClaimQuantity;
  measured?: number;
  uncertainty?: number;
  band?: AcceptanceBand;
  interval?: [number, number];
  instrument?: string;
  raw_sha256?: string;
  row: LedgerRow;
  stale_reason?: StaleReason | "entry_removed";
  /** The row the receipt recorded, when it differs from now. */
  receipt_row?: string;
}

export interface Scorecard {
  schema: "vcad.scorecard/1";
  rows: ScorecardRow[];
  counts: Record<LedgerRow, number>;
  markdown: string;
}

function fmt(v: unknown): string {
  if (typeof v !== "number") return v === undefined ? "—" : String(v);
  if (!Number.isFinite(v)) return String(v);
  const a = Math.abs(v);
  return a !== 0 && (a >= 1e6 || a < 1e-3) ? v.toExponential(3) : String(Number(v.toPrecision(4)));
}

function bandText(b?: AcceptanceBand): string {
  if (!b) return "—";
  return b.kind === "factor" ? `×/÷${fmt(b.band_factor)}` : `±${fmt(b.half_width)}`;
}

const ROW_LABEL: Record<string, string> = {
  holds: "✅ Holds",
  violated: "❌ Violated",
  unmeasured: "⏳ Unmeasured",
  stale: "⚠️ Stale",
};

/** Build the scorecard from receipt-ordered (entry, state) pairs. */
export function renderScorecard(
  items: Array<{ claim: ReceiptClaim; entry?: LedgerEntry; state: RowState; receiptRow?: string }>,
): Scorecard {
  const counts: Record<LedgerRow, number> = { holds: 0, violated: 0, unmeasured: 0, stale: 0 };
  const rows: ScorecardRow[] = items.map(({ claim, entry, state, receiptRow }, i) => {
    counts[state.row] += 1;
    const m = entry?.measurement;
    return {
      n: i + 1,
      claim: claim.id,
      ...(claim.subject ? { subject: claim.subject } : {}),
      domain: claim.domain,
      oracle: `${claim.oracle.id}@${claim.oracle.version}`,
      predicted: entry?.predicted ?? claim.predicted ?? { value: "—" },
      ...(m ? { measured: m.value, uncertainty: m.uncertainty, band: m.band, instrument: m.instrument } : {}),
      ...(m?.raw_sha256 ? { raw_sha256: m.raw_sha256 } : {}),
      ...(state.interval ? { interval: state.interval } : {}),
      row: state.row,
      ...(state.stale_reason ? { stale_reason: state.stale_reason } : {}),
      ...(!entry ? { stale_reason: "entry_removed" as const } : {}),
      ...(receiptRow && receiptRow !== state.row ? { receipt_row: receiptRow } : {}),
    };
  });
  const head =
    "| # | claim | domain | predicted | measured | band | verdict | instrument |\n" +
    "|---|---|---|---|---|---|---|---|";
  const lines = rows.map((r) => {
    const unit = r.predicted.unit ? ` ${r.predicted.unit}` : "";
    const meas = r.measured === undefined ? "—" : `${fmt(r.measured)}${r.uncertainty ? ` ± ${fmt(r.uncertainty)}` : ""}${unit}`;
    const name = r.subject ? `\`${r.claim}\` (${r.subject})` : `\`${r.claim}\``;
    const verdict = ROW_LABEL[r.row] + (r.stale_reason ? ` (${r.stale_reason.replace("_", " ")})` : "");
    const inst = (r.instrument ?? "—").replace(/\|/g, "/");
    return `| ${r.n} | ${name} | ${r.domain} | ${fmt(r.predicted.value)}${unit} | ${meas} | ${bandText(r.band)} | ${verdict} | ${inst} |`;
  });
  const tally = `**${counts.holds} hold · ${counts.violated} violated · ${counts.unmeasured} unmeasured · ${counts.stale} stale** of ${rows.length}`;
  return { schema: "vcad.scorecard/1", rows, counts, markdown: [head, ...lines, "", tally].join("\n") };
}

/** Scorecard over the document's current ledger, in ledger order. */
export async function documentScorecard(doc: Document): Promise<Scorecard> {
  const ledger = ledgerOf(doc);
  const fp = await designFingerprint(doc);
  const items = [];
  for (const e of ledger?.entries ?? []) {
    const state = await classifyEntry(e, fp);
    items.push({ claim: ledgerReceiptClaim(e, state), entry: e, state });
  }
  return renderScorecard(items);
}

export type LedgerVerifyStatus = "Holds" | "Stale" | "Violated";

/**
 * Re-check a receipt's ledger claims against the document's current ledger,
 * in the receipt's claim order. A row whose entry is gone reads Stale; a
 * row whose verdict moved since the receipt (e.g. Unmeasured → Holds) makes
 * the receipt Stale (rebuild it); any Violated row violates it.
 */
export async function verifyLedgerClaims(
  doc: Document,
  receipt: { claims: ReceiptClaim[] },
): Promise<{ status: LedgerVerifyStatus; scorecard: Scorecard; unreceipted: string[] }> {
  const ledger = ledgerOf(doc);
  const fp = await designFingerprint(doc);
  const byKey = new Map((ledger?.entries ?? []).map((e) => [keyOfEntry(e), e] as const));
  const covered = new Set<string>();
  const items: Array<{ claim: ReceiptClaim; entry?: LedgerEntry; state: RowState; receiptRow?: string }> = [];
  let moved = false;
  for (const c of receipt.claims) {
    const d = ledgerDetails(c);
    if (!d) continue;
    const key = String(d.key ?? entryKey(c.id, c.subject));
    covered.add(key);
    const entry = byKey.get(key);
    const receiptRow = typeof d.row === "string" ? d.row : undefined;
    if (!entry) {
      items.push({ claim: c, state: { key, row: "stale" }, receiptRow });
      continue;
    }
    const state = await classifyEntry(entry, fp);
    // The prediction the receipt certified must be the one frozen now.
    if (d.prediction_sha256 !== entry.prediction_sha256 && state.row !== "stale") {
      state.row = "stale";
      state.stale_reason = "prediction_changed";
      delete state.interval;
    }
    if (receiptRow !== state.row) moved = true;
    items.push({ claim: ledgerReceiptClaim(entry, state), entry, state, receiptRow });
  }
  const unreceipted = [...byKey.keys()].filter((k) => !covered.has(k));
  const scorecard = renderScorecard(items);
  const status: LedgerVerifyStatus =
    scorecard.counts.violated > 0
      ? "Violated"
      : scorecard.counts.stale > 0 || moved || unreceipted.length > 0
        ? "Stale"
        : "Holds";
  return { status, scorecard, unreceipted };
}

// -------------------------------------------------------------- importers

/** Raw instrument file: inline `text`, or a `path` under the server root. */
async function readRaw(args: Json): Promise<{ text: string; sha256: string; name?: string }> {
  let text: string;
  let name: string | undefined;
  if (typeof args.text === "string") {
    text = args.text;
    name = typeof args.file_name === "string" ? args.file_name : undefined;
  } else if (typeof args.path === "string") {
    const { resolveWithinRoot } = await import("./safe-path.js");
    const { readFile } = await import("node:fs/promises");
    const abs = resolveWithinRoot(args.path);
    text = await readFile(abs, "utf8");
    name = args.path;
  } else {
    throw new Error("import_measurement: pass the instrument file as `text` (contents) or `path`");
  }
  return { text, sha256: await sha256Hex(text), ...(name ? { name } : {}) };
}

/** Engine surface the importers need (the parsers live in the kernel). */
export interface MeasurementEngine {
  importMeasurementFile(format: string, text: string, contextJson: string): unknown;
}

/** NanoVNA tolerance overrides (defaults: 2 dB, 2%, 10 ohm, 25%). */
type NanoVnaTolerances = { s11_db?: number; freq_rel?: number; z_ohm?: number; bandwidth_rel?: number };

const TEMP_UNITS = new Set(["C", "degC", "°C"]);

/** Run a typed importer (kernel-side, fail-closed parser) and record its
 *  readings on the ledger. */
export async function importMeasurement(doc: Document, args: Json, engine: MeasurementEngine) {
  const format = String(args.format ?? "");
  const ledger = ledgerOf(doc);
  if (!ledger || ledger.entries.length === 0) {
    throw new Error("import_measurement: this document has no claims ledger — attach solver claims first (attach_claims)");
  }
  if (!["touchstone_s1p", "rew_impedance", "thermocouple_csv"].includes(format)) {
    throw new Error(`import_measurement: unknown format ${JSON.stringify(format)} (touchstone_s1p, rew_impedance, thermocouple_csv)`);
  }
  const raw = await readRaw(args);
  const subject = typeof args.subject === "string" ? args.subject : undefined;
  const instrument = typeof args.instrument === "string" && args.instrument.trim() ? args.instrument : undefined;
  const base = { raw_sha256: raw.sha256, ...(raw.name ? { raw_name: raw.name } : {}), importer: format };
  const replace = args.replace === true;
  let inputs: MeasurementInput[];
  let derived: Json;

  if (format === "touchstone_s1p") {
    const antenna = ledger.entries.filter(
      (e) => e.claim.domain === "antenna" && (subject === undefined || (e.claim.subject ?? "") === subject),
    );
    const setIdx = [...new Set(antenna.map((e) => e.claim_set).filter((i): i is number => typeof i === "number"))];
    if (setIdx.length !== 1) {
      throw new Error(
        setIdx.length === 0
          ? "import_measurement: no antenna claim set on the ledger — attach the full analyze_antenna result (it carries the band and reference impedance)"
          : "import_measurement: several antenna claim sets are attached — pass `subject` to pick one",
      );
    }
    const claimSet = JSON.parse(ledger.claim_sets![setIdx[0]!]!.json) as Json;
    const r = engine.importMeasurementFile(
      format,
      raw.text,
      JSON.stringify({ claim_set: claimSet, ...(args.tolerances ? { tolerances: args.tolerances as NanoVnaTolerances } : {}) }),
    ) as { points: number; reference_ohm: number; measurements: Array<{ claim: string; value: number; tolerance: number }> };
    const present = new Set(antenna.map((e) => e.claim.id));
    const used = r.measurements.filter((m) => present.has(`antenna.${m.claim}`));
    const skipped = r.measurements.filter((m) => !present.has(`antenna.${m.claim}`)).map((m) => m.claim);
    if (used.length === 0) throw new Error("import_measurement: the sweep matched none of the attached antenna claims");
    inputs = used.map((m) => {
      const e = antenna.find((x) => x.claim.id === `antenna.${m.claim}`)!;
      return {
        claim_id: e.claim.id,
        ...(e.claim.subject ? { subject: e.claim.subject } : {}),
        value: m.value,
        ...(e.predicted.unit ? { unit: e.predicted.unit } : {}),
        uncertainty: 0,
        instrument: instrument ?? `one-port VNA (.s1p, ${r.points} points, z0 ${r.reference_ohm} ohm)`,
        band: { kind: "absolute", half_width: m.tolerance },
        ...base,
      };
    });
    derived = {
      points: r.points,
      reference_ohm: r.reference_ohm,
      measurements: used,
      ...(skipped.length ? { skipped_not_attached: skipped } : {}),
      note: "gain and efficiency stay Unmeasured: a reflection-only instrument cannot see them",
    };
  } else if (format === "rew_impedance") {
    const claimId = typeof args.claim_id === "string" ? args.claim_id : "acoustics.tuning_hz";
    const band = parseBand(args.band, "acoustics", claimId);
    const r = engine.importMeasurementFile(
      format,
      raw.text,
      JSON.stringify({ extra_uncertainty_hz: Number(args.uncertainty ?? 0) }),
    ) as { f_b_hz: number; uncertainty_hz: number };
    inputs = [
      {
        claim_id: claimId,
        ...(subject ? { subject } : {}),
        value: r.f_b_hz,
        unit: "Hz",
        uncertainty: r.uncertainty_hz,
        instrument: instrument ?? "REW impedance sweep (vented-box saddle between the two |Z| peaks)",
        band,
        ...base,
      },
    ];
    derived = { ...r };
  } else {
    if (typeof args.claim_id !== "string") throw new Error("import_measurement: thermocouple_csv needs `claim_id` (e.g. thermal.t_max_c)");
    const e = findEntry(ledger, args.claim_id, subject);
    if (!TEMP_UNITS.has(String(e.predicted.unit ?? ""))) {
      throw new Error(`${args.claim_id} is in ${JSON.stringify(e.predicted.unit ?? "1")}, not a temperature — a thermocouple log can only measure a temperature claim`);
    }
    if (typeof args.instrument_uncertainty_c !== "number") {
      throw new Error("import_measurement: thermocouple_csv needs instrument_uncertainty_c (type-K standard limits are ±2.2 °C)");
    }
    const band = parseBand(args.band, e.claim.domain, args.claim_id);
    const r = engine.importMeasurementFile(
      format,
      raw.text,
      JSON.stringify({
        instrument_uncertainty_c: args.instrument_uncertainty_c,
        ...(typeof args.column === "string" ? { column: args.column } : {}),
        ...(typeof args.reduce === "string" ? { reduce: args.reduce } : {}),
        ...(typeof args.window_s === "number" ? { window_s: args.window_s } : {}),
        ...(typeof args.max_drift_c === "number" ? { max_drift_c: args.max_drift_c } : {}),
      }),
    ) as { value_c: number; uncertainty_c: number; samples: number; column: string };
    inputs = [
      {
        claim_id: args.claim_id,
        ...(e.claim.subject ? { subject: e.claim.subject } : {}),
        value: r.value_c,
        unit: String(e.predicted.unit),
        uncertainty: r.uncertainty_c,
        instrument: instrument ?? `thermocouple log (${r.column}, ${r.samples} samples)`,
        band,
        ...base,
      },
    ];
    derived = { ...r };
  }
  const res = await recordClaimMeasurements(doc, inputs, { replace });
  return { format, raw_sha256: raw.sha256, derived, ...res };
}

// ------------------------------------------------------------------ tools

function textResult(payload: unknown, structured?: Json) {
  return {
    content: [{ type: "text" as const, text: JSON.stringify(payload) }],
    ...(structured ? { structuredContent: structured } : {}),
  };
}

function errorResult(e: unknown) {
  return {
    content: [{ type: "text" as const, text: `Error: ${e instanceof Error ? e.message : String(e)}` }],
    isError: true,
  };
}

const bandSchema = {
  type: "object" as const,
  description:
    "Acceptance band the measurer commits to. Thermal / EM / acoustics / flow use {band_factor} " +
    "(holds when measured ∈ [p/band − u, p·band + u]); antenna / tolerance use {half_width} " +
    "(holds when |measured − p| ≤ half_width + u [+ the prediction's own u]). Required — vcad never invents one.",
  properties: {
    band_factor: { type: "number" as const },
    half_width: { type: "number" as const },
  },
};

/** Parse `record_measurement { claim_id | claim_measurements }` input. */
export function parseMeasurementArgs(args: Json): MeasurementInput[] {
  const list: Json[] = Array.isArray(args.claim_measurements)
    ? (args.claim_measurements as Json[])
    : [args];
  return list.map((m) => {
    const id = typeof m.claim_id === "string" ? m.claim_id : "";
    if (!id) throw new Error("each claim measurement needs claim_id");
    // Domain shape is checked against the entry in recordClaimMeasurements.
    const band = m.band as Json | undefined;
    if (!band || typeof band !== "object") {
      throw new Error(`measurement for ${id} needs a band: {band_factor} or {half_width}`);
    }
    return {
      claim_id: id,
      ...(typeof m.subject === "string" ? { subject: m.subject } : {}),
      value: Number(m.value),
      ...(typeof m.unit === "string" ? { unit: m.unit } : {}),
      uncertainty: m.uncertainty === undefined ? Number.NaN : Number(m.uncertainty),
      instrument: typeof m.instrument === "string" ? m.instrument : "",
      band:
        band.kind === "absolute" || (band.kind === undefined && band.half_width !== undefined)
          ? { kind: "absolute", half_width: Number(band.half_width) }
          : { kind: "factor", band_factor: Number(band.band_factor) },
      ...(typeof m.raw_sha256 === "string" ? { raw_sha256: m.raw_sha256 } : {}),
      ...(typeof m.raw_name === "string" ? { raw_name: m.raw_name } : {}),
    };
  });
}

/** `record_measurement` in claim mode (dispatched from print-check). */
export async function recordMeasurementForClaims(args: Json) {
  try {
    const { doc, documentId } = resolveDocInput(args);
    const inputs = parseMeasurementArgs(args);
    const res = await recordClaimMeasurements(doc, inputs, { replace: args.replace === true });
    const scorecard = await documentScorecard(doc);
    return textResult({ ...res, scorecard }, { ...res, scorecard, ...(documentId ? { document_id: documentId } : {}) });
  } catch (e) {
    return errorResult(e);
  }
}

/** Extra `record_measurement` properties for claim mode. */
export const claimMeasurementSchemaProps = {
  claim_id: {
    type: "string" as const,
    description:
      "Claim mode: the ledger claim this reading measures (e.g. thermal.t_max_c, antenna.resonant_frequency). With claim_id, `value`, `uncertainty`, `instrument`, and `band` are required; `measurements` is not used. A claim_id that is not on the document's claims ledger is an error.",
  },
  subject: { type: "string" as const, description: "Claim mode: disambiguates a claim attached under several subjects/labels." },
  value: { type: "number" as const, description: "Claim mode: measured value, in the prediction's unit." },
  unit: { type: "string" as const, description: "Claim mode: unit (must match the prediction's unit)." },
  uncertainty: { type: "number" as const, description: "Claim mode: one-sigma absolute uncertainty, same unit." },
  instrument: { type: "string" as const, description: "Claim mode: what measured it, and how." },
  band: bandSchema,
  raw_sha256: { type: "string" as const, description: "Claim mode: SHA-256 of the raw instrument file, for the audit trail." },
  claim_measurements: {
    type: "array" as const,
    description: "Claim mode, several at once (all-or-nothing): [{claim_id, value, uncertainty, instrument, band, unit?, subject?, raw_sha256?}].",
    items: { type: "object" as const },
  },
  replace: { type: "boolean" as const, description: "Claim mode: overwrite an existing measurement." },
};

export const toolDefs: ToolDef[] = [
  {
    name: "attach_claims",
    pack: null,
    description:
      "Freeze solver predictions onto the document's claims ledger (vcad.claims-ledger/1) so ONE receipt carries every domain: " +
      "pass the result of solve_thermal, analyze_antenna, simulate_em, analyze_structure, beam_check, analyze_tolerance_stackup, " +
      "simulate_flow, parameter_gradient — anything returning `receipt_claims` or a unified claim array (e.g. acoustics claims " +
      "from vcad-kernel-acoustics, which has no MCP solver yet). Each claim " +
      "keeps its oracle id/version and solver provenance; the numeric prediction, a prediction hash, and the design fingerprint are " +
      "frozen. Predictions roll up Provisional, never Pass, until record_measurement / import_measurement binds hardware readings. " +
      "Re-attaching a different value for a frozen claim is refused unless supersede:true (a measured row then reads Stale). " +
      "Use `label` to attach two runs of one solver. build_receipt then includes every ledger row.",
    inputSchema: {
      type: "object" as const,
      required: ["claims"],
      properties: {
        document_id: { type: "string" as const, description: "Session id of the document that owns the ledger." },
        claims: {
          description: "A solver result (with receipt_claims / claim_set), an array of results, or an array of unified receipt claims.",
        },
        source_tool: { type: "string" as const, description: "The tool that produced the claims (e.g. solve_thermal) — provenance." },
        label: { type: "string" as const, description: "Subject for claims that carry none, e.g. 'driver_coil' — namespaces repeat runs." },
        supersede: { type: "boolean" as const, description: "Replace already-frozen predictions (measured rows then read Stale)." },
      },
    },
    handler: async (args) => {
      try {
        const a = args as Json;
        const { doc, documentId } = resolveDocInput(a);
        const res = await attachClaims(doc, a.claims, {
          ...(typeof a.source_tool === "string" ? { source_tool: a.source_tool } : {}),
          ...(typeof a.label === "string" ? { label: a.label } : {}),
          ...(a.supersede === true ? { supersede: true } : {}),
        });
        return textResult(res, { ...res, ...(documentId ? { document_id: documentId } : {}) });
      } catch (e) {
        return errorResult(e);
      }
    },
    // Writes Document.claims_ledger: persist, undo snapshot, live-app push.
    behavior: behavior({ writesDoc: true }),
  },
  {
    name: "import_measurement",
    pack: null,
    description:
      "Import a raw instrument file and bind its readings to the document's frozen predictions (claims ledger), fail-closed. " +
      "Formats: `touchstone_s1p` (NanoVNA one-port sweep → antenna s11/z_in/resonance/bandwidth claims, NanoVNA default " +
      "tolerances, overridable via `tolerances`), `rew_impedance` (REW 'export impedance as text' → acoustics port tuning f_b " +
      "= the |Z| saddle between the two peaks; needs `band`), `thermocouple_csv` (header row with a time column + a temperature " +
      "column → steady-state mean of the last window, refused if still drifting; or reduce:'max' for a peak claim; needs " +
      "`claim_id`, `band`, `instrument_uncertainty_c`). The raw file's SHA-256 is recorded with every reading. Returns the " +
      "updated scorecard (Holds / Violated / Unmeasured / Stale per claim).",
    inputSchema: {
      type: "object" as const,
      required: ["format"],
      properties: {
        document_id: { type: "string" as const },
        format: { type: "string" as const, enum: ["touchstone_s1p", "rew_impedance", "thermocouple_csv"] },
        text: { type: "string" as const, description: "The instrument file's contents." },
        path: { type: "string" as const, description: "Or a path to it, relative to the server's working directory." },
        file_name: { type: "string" as const, description: "Name to record for an inline `text` file." },
        claim_id: { type: "string" as const, description: "Target claim (rew_impedance defaults to acoustics.tuning_hz; required for thermocouple_csv)." },
        subject: { type: "string" as const, description: "Disambiguates a claim attached under several labels." },
        band: bandSchema,
        uncertainty: { type: "number" as const, description: "rew_impedance: extra one-sigma uncertainty, Hz (combined with the sweep grid)." },
        instrument_uncertainty_c: { type: "number" as const, description: "thermocouple_csv: probe + meter one-sigma uncertainty, °C." },
        column: { type: "string" as const, description: "thermocouple_csv: temperature column name when the header has several." },
        reduce: { type: "string" as const, enum: ["steady", "max"], description: "thermocouple_csv: steady-state mean (default) or peak." },
        window_s: { type: "number" as const, description: "thermocouple_csv: steady-state window, s (default last 10% of the log)." },
        max_drift_c: { type: "number" as const, description: "thermocouple_csv: allowed drift over the window, °C (default 0.5)." },
        tolerances: {
          type: "object" as const,
          description: "touchstone_s1p: {s11_db, freq_rel, z_ohm, bandwidth_rel} overriding the NanoVNA defaults (2 dB, 2%, 10 ohm, 25%).",
        },
        instrument: { type: "string" as const, description: "Instrument provenance override." },
        replace: { type: "boolean" as const, description: "Overwrite existing measurements." },
      },
    },
    handler: async (args, ctx) => {
      try {
        const a = args as Json;
        const { doc, documentId } = resolveDocInput(a);
        const res = await importMeasurement(doc, a, ctx.engine);
        const scorecard = await documentScorecard(doc);
        return textResult({ ...res, scorecard }, { ...res, scorecard, ...(documentId ? { document_id: documentId } : {}) });
      } catch (e) {
        return errorResult(e);
      }
    },
    // Writes Document.claims_ledger: persist, undo snapshot, live-app push.
    behavior: behavior({ writesDoc: true }),
  },
];
