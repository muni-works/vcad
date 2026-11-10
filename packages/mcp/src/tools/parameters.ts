/**
 * Parametric-parameter tools — the differentiable seam over MCP.
 *
 *  - `list_parameters` reads a document's named parameters (raw expression,
 *    resolved value, unit, scrub bounds).
 *  - `set_parameters` batch-updates parameter values and reports a `changed`
 *    diff of the deltas. A document writer: goes through the standard session
 *    pipeline (undo snapshot, integrity, durable persist) via the dispatch
 *    layer.
 *  - `parameter_gradient` differentiates the mass-property + bounding-box
 *    QoIs with respect to one named parameter (`d QoI / dθ`) through the
 *    Rust differentiable seam — a capability no other CAD MCP exposes.
 *
 * `optimize_parameters` (solve params for a target QoI) is deferred: the
 * kernel ships an L-BFGS optimizer, but wiring a *document* parameter into it
 * (build closure + seeding synthesis + objective) is new Rust API design.
 * See the follow-up issue.
 */

import type {
  Engine,
  PartParameterGradient,
  SensitivityReport,
  SensitivityRequest,
} from "@vcad/engine";
import { behavior, type ToolDef } from "./tool-def.js";
import { resolveParameters, solveDesignConstraints } from "@vcad/engine";
import type { Document, Expr, Parameter } from "@vcad/ir";
import { getPcbNodeIds } from "@vcad/core";
import { appendIntegrity, computeIntegrity } from "./integrity.js";
import { carryElectronics } from "./carry-electronics.js";
import {
  getSession,
  resolveDocInput,
  recordTriangles,
} from "./session-core.js";

/** MCP result shape used across these tools. */
type ToolResult = {
  content: Array<{ type: "text"; text: string }>;
  structuredContent?: Record<string, unknown>;
  isError?: boolean;
};

function jsonResult(payload: unknown): ToolResult {
  return {
    content: [{ type: "text", text: JSON.stringify(payload, null, 2) }],
  };
}

// ─── list_parameters ─────────────────────────────────────────────────────────

export const listParametersSchema = {
  type: "object" as const,
  properties: {
    document_id: {
      type: "string" as const,
      description: "Session id from open_document.",
    },
    document: {
      type: "object" as const,
      description:
        "Inline Document IR to read instead of a session (stateless path).",
    },
  },
};

interface ParameterInfo {
  name: string;
  value: Expr;
  resolved: number | null;
  unit?: string;
  min?: number;
  max?: number;
  description?: string;
}

export function listParameters(input: unknown): ToolResult {
  const args = (input ?? {}) as Record<string, unknown>;
  const { doc } = resolveDocInput(args);

  const params = doc.parameters ?? {};
  let resolved: Record<string, number> = {};
  try {
    resolved = resolveParameters(params);
  } catch {
    // A malformed/cyclic formula shouldn't sink the listing — report raw
    // values with `resolved: null` so the agent can still see and fix them.
    resolved = {};
  }

  const parameters: ParameterInfo[] = Object.entries(params).map(
    ([name, p]) => {
      const param = p as Parameter;
      const info: ParameterInfo = {
        name,
        value: param.value,
        resolved: name in resolved ? resolved[name] : null,
      };
      if (param.unit !== undefined) info.unit = param.unit;
      if (param.min !== undefined) info.min = param.min;
      if (param.max !== undefined) info.max = param.max;
      if (param.description !== undefined) info.description = param.description;
      return info;
    },
  );

  return jsonResult({ count: parameters.length, parameters });
}

// ─── set_parameters ────────────────────────────────────────────────────────────

export const setParametersSchema = {
  type: "object" as const,
  properties: {
    document_id: {
      type: "string" as const,
      description: "Session id from open_document (required to persist).",
    },
    parameters: {
      type: "object" as const,
      description:
        "Map of parameter name → new numeric value, e.g. { \"r\": 12, \"h\": 8 }. " +
        "Every name must already be declared in the document's parameters.",
      additionalProperties: { type: "number" as const },
    },
  },
  required: ["document_id", "parameters"],
};

/**
 * Every parameter name something in the document actually reads: binding
 * formulas plus design-constraint dimensions. A name outside this set is
 * declared but inert — setting it is a no-op.
 */
function referencedNames(doc: Document): Set<string> {
  const out = new Set<string>();
  const harvest = (expr: unknown) => {
    if (typeof expr !== "string") return;
    for (const v of expr.match(/[A-Za-z_]\w*/g) ?? []) out.add(v);
  };
  for (const expr of Object.values(doc.bindings ?? {})) harvest(expr);
  // Constraint dimensions are expressions over parameters too ("board_width
  // - 2*margin"), and set_parameters re-solves them below.
  for (const c of doc.constraints ?? []) {
    for (const v of Object.values(c as unknown as Record<string, unknown>)) harvest(v);
  }
  return out;
}

/** Canonical JSON (sorted keys): a kernel's map ordering is not stable. */
function canonical(value: unknown): string {
  return JSON.stringify(value, (_k, v: unknown) =>
    v && typeof v === "object" && !Array.isArray(v)
      ? Object.fromEntries(
          Object.entries(v as Record<string, unknown>).sort(([a], [b]) => (a < b ? -1 : 1)),
        )
      : v,
  );
}

const geometryOf = (d: Document) =>
  canonical({ nodes: d.nodes, roots: d.roots, defs: d.partDefs, inst: d.instances, joints: d.joints });

/** A loon literal: always with a decimal point, never in exponent notation. */
function loonNumber(v: number): string {
  const text = String(v);
  if (text.includes("e")) return v.toFixed(12);
  return text.includes(".") ? text : `${text}.0`;
}

/** The source with these `[defparam name <number>` values, or null if one isn't there. */
function sourceWithValues(text: string, values: Record<string, number>): string | null {
  let out = text;
  for (const [name, value] of Object.entries(values)) {
    const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const form = new RegExp(`(\\[\\s*defparam\\s+${escaped}\\s+)-?(?:\\d+\\.?\\d*|\\.\\d+)(?:[eE][-+]?\\d+)?`, "g");
    if (!form.test(out)) return null;
    out = out.replace(form, (_m, head: string) => head + loonNumber(value));
  }
  return out;
}

/**
 * Re-run the document's loon source at given parameter values, or null when
 * that isn't possible. Binding recovery can only express affine dependence on
 * addressable fields, so a parameter reaching sketch points or non-linear
 * arithmetic gets no binding; the source still knows how it drives geometry.
 *
 * Direct edits don't round-trip to loon, so a diverged source is off limits —
 * except divergence from set_parameters alone, which this reproduces by
 * substituting every current value.
 */
function sourceEvaluator(
  doc: Document,
  engine: Engine,
): ((values: Record<string, number>) => Document | null) | null {
  const src = doc.source;
  if (!src || src.language !== "loon") return null;
  if (src.diverged && (src.diverged_by ?? []).some((t) => t !== "set_parameters")) return null;
  return (values) => {
    const text = sourceWithValues(src.text, values);
    if (text === null) return null;
    try {
      const evaluated = engine.evalLoonDocument(text, src.modules ?? {}, {
        baseDir: src.base_dir ?? undefined,
      });
      if (evaluated) resolveAssets?.(evaluated, src.base_dir ?? undefined, engine);
      return evaluated;
    } catch {
      return null;
    }
  };
}

type AssetResolver = (doc: Document, baseDir: string | undefined, engine: Engine) => void;
let resolveAssets: AssetResolver | null = null;

/**
 * Give set_parameters a way to read a loon program's mesh and STEP imports.
 * A hook because that means the filesystem and this module also ships in the
 * browser tool set, where there are no files to resolve.
 */
export function setParameterAssetResolver(resolver: AssetResolver): void {
  resolveAssets = resolver;
}

interface ParameterDelta {
  name: string;
  previous: Expr;
  value: number;
}

export async function setParameters(input: unknown, engine: Engine): Promise<ToolResult> {
  const args = (input ?? {}) as Record<string, unknown>;
  const documentId = String(args.document_id ?? "");
  if (!documentId) {
    throw new Error("`document_id` is required for set_parameters.");
  }
  const updates = args.parameters;
  if (!updates || typeof updates !== "object" || Array.isArray(updates)) {
    throw new Error(
      "`parameters` must be an object mapping parameter name → numeric value.",
    );
  }

  const doc: Document = getSession(documentId);
  doc.parameters ??= {};
  // Same object, but a `const` binding: the `??=` narrowing above is lost
  // inside the closures below, since TypeScript can't assume a mutable
  // property stays defined across a callback boundary.
  const params = doc.parameters;

  // Validate up front so a bad entry never leaves a half-applied batch.
  const entries = Object.entries(updates as Record<string, unknown>);
  const unknown: string[] = [];
  for (const [name, value] of entries) {
    if (!(name in doc.parameters)) unknown.push(name);
    else if (typeof value !== "number" || !Number.isFinite(value)) {
      throw new Error(
        `Parameter "${name}" must be set to a finite number (got ${JSON.stringify(value)}).`,
      );
    }
  }
  if (unknown.length > 0) {
    throw new Error(
      `Unknown parameter(s): ${unknown.join(", ")}. Use list_parameters to see declared names.`,
    );
  }

  // A parameter only moves geometry if something references it: a binding
  // formula, or a design-constraint dimension. Setting one that nothing
  // references succeeds and changes nothing — the failure mode is turning a
  // knob and watching the model sit still. Typically it is a *derived*
  // parameter (a stack boundary follows from thicknesses and gaps; geometry
  // is bound to those), so the report names what to set instead.
  const referenced = referencedNames(doc);
  const unbound = entries.map(([name]) => name).filter((name) => !referenced.has(name));

  // An unbound base parameter may still drive the model through its source.
  // Probe one at a time, so a batch can't credit an inert name with another's
  // effect; a parameter already at the requested value is nudged for the probe.
  const evaluate = sourceEvaluator(doc, engine);
  const numericValues = (over: Record<string, number> = {}) => ({
    ...Object.fromEntries(
      Object.entries(params).flatMap(([n, p]) =>
        typeof (p as Parameter).value === "number" ? [[n, (p as Parameter).value as number]] : [],
      ),
    ),
    ...over,
  });
  const candidates = unbound.filter((n) => typeof (params[n] as Parameter).value === "number");
  const baseline = evaluate && candidates.length ? evaluate(numericValues()) : null;
  const sourceDriven = new Set<string>();
  if (evaluate && baseline) {
    const reference = geometryOf(baseline);
    for (const name of candidates) {
      const current = (params[name] as Parameter).value as number;
      const target = (updates as Record<string, number>)[name]!;
      const probe = evaluate(
        numericValues({ [name]: target !== current ? target : current + Math.max(1, Math.abs(current)) * 0.01 }),
      );
      if (probe && geometryOf(probe) !== reference) sourceDriven.add(name);
    }
  }

  const unboundReason = (name: string, isDerived: boolean, expr: unknown): string => {
    if (isDerived) return `derived (${JSON.stringify(expr)}) — no field is bound to it`;
    if (baseline) {
      return "the loon source declares it, but re-evaluating the source at a new value changes no geometry — nothing in the model reads it";
    }
    if (doc.source) {
      return (
        "no field is bound to it: the loon expression reaching geometry can't be captured as a " +
        "formula (sketch points or non-linear arithmetic), and the source can't be re-run here " +
        "(the session was edited directly, or the parameter isn't a literal defparam in the " +
        "source). Change it in the source and re-run create_cad_loon."
      );
    }
    return "no field is bound to it";
  };

  const noEffect = unbound
    .filter((name) => !sourceDriven.has(name))
    .map((name) => {
      const value = (params[name] as Parameter).value;
      const inputs =
        typeof value === "string"
          ? [...new Set(value.match(/[A-Za-z_]\w*/g) ?? [])].filter(
              (v) => v in params && referenced.has(v),
            )
          : [];
      return {
        name,
        reason: unboundReason(name, typeof value === "string", value),
        ...(inputs.length ? { set_instead: inputs } : {}),
      };
    });

  // Source-driven parameters have no binding to apply: rebuild from source at
  // the new values, before anything is touched so a failure changes nothing.
  // The session's board comes along unless the source authored it; `source`
  // stays the authored text, as a binding-only change would leave it.
  let rebuilt: Document | null = null;
  if (evaluate && sourceDriven.size > 0) {
    rebuilt = evaluate(numericValues(updates as Record<string, number>));
    if (!rebuilt) {
      throw new Error(
        `Could not re-evaluate the loon source for: ${[...sourceDriven].join(", ")}. Nothing was changed.`,
      );
    }
    const authorsBoard =
      getPcbNodeIds(rebuilt).length > 0 ||
      Boolean((rebuilt as Document & { pcb?: unknown }).pcb) ||
      Boolean(rebuilt.schematic);
    if (!authorsBoard) carryElectronics(doc, rebuilt);
  }

  const changed: ParameterDelta[] = [];
  for (const [name, value] of entries) {
    const param = doc.parameters[name] as Parameter;
    changed.push({ name, previous: param.value, value: value as number });
    param.value = value as number;
  }

  if (rebuilt) {
    const source = doc.source;
    for (const key of Object.keys(doc)) delete (doc as unknown as Record<string, unknown>)[key];
    Object.assign(doc, rebuilt, { source });
  }

  // Parameters drive constraint dimensions ("board_width - 2*margin") —
  // re-solve the document's design constraints so a parameter change
  // relayouts everything bound to it in the same call.
  let constraintSolve: Record<string, unknown> | undefined;
  if ((doc.constraints ?? []).length > 0) {
    const outcome = await solveDesignConstraints(doc);
    if (outcome.status === "ok") {
      doc.nodes = outcome.value.document.nodes;
      doc.constraints = outcome.value.document.constraints;
      constraintSolve = outcome.value.report as unknown as Record<string, unknown>;
    } else {
      constraintSolve = { unavailable: outcome.reason };
    }
  }

  const result: ToolResult = jsonResult({
    document_id: documentId,
    updated: changed.length,
    changed,
    ...(rebuilt ? { reevaluated_from_source: [...sourceDriven] } : {}),
    ...(noEffect.length ? { no_effect: noEffect } : {}),
    ...(constraintSolve ? { constraint_solve: constraintSolve } : {}),
  });
  result.structuredContent = { changed };

  // Geometry changed under the new parameter values — carry an integrity
  // certificate like every other mutation.
  const integrity = computeIntegrity(doc, engine);
  if (integrity) {
    appendIntegrity(result, integrity);
    recordTriangles(documentId, integrity.triangles);
  }

  return result;
}

// ─── parameter_gradient ─────────────────────────────────────────────────────────

export const parameterGradientSchema = {
  type: "object" as const,
  properties: {
    document_id: {
      type: "string" as const,
      description: "Session id from open_document.",
    },
    document: {
      type: "object" as const,
      description:
        "Inline Document IR to differentiate instead of a session (stateless path).",
    },
    parameter: {
      type: "string" as const,
      description:
        "Name of the parameter to differentiate. Must be declared in the " +
        "document's parameters and bound onto some geometry field.",
    },
    density: {
      type: "number" as const,
      description:
        "Density fed to the mass integrals (mass = density · volume). Defaults to 1 (raw volume moments).",
    },
    probe_step: {
      type: "number" as const,
      description:
        "Finite step for seeding synthesis (surface matching). The reported " +
        "volume/mass/centroid derivatives are analytic, not finite differences. Defaults to 1e-4.",
    },
  },
  required: ["parameter"],
};

export function parameterGradient(input: unknown, engine: Engine): ToolResult {
  const args = (input ?? {}) as Record<string, unknown>;
  const parameter = args.parameter;
  if (typeof parameter !== "string" || parameter.length === 0) {
    throw new Error("`parameter` (the parameter name to differentiate) is required.");
  }
  const { doc } = resolveDocInput(args);

  const density =
    typeof args.density === "number" ? (args.density as number) : 1.0;
  const probeStep =
    typeof args.probe_step === "number" ? (args.probe_step as number) : 0;

  const parts: PartParameterGradient[] = engine.parameterGradient(
    doc,
    parameter,
    { density, probeStep },
  );

  return jsonResult({ parameter, density, parts });
}

// ─── sensitivity ────────────────────────────────────────────────────────────────

export const sensitivitySchema = {
  type: "object" as const,
  properties: {
    document_id: {
      type: "string" as const,
      description: "Session id from open_document.",
    },
    document: {
      type: "object" as const,
      description:
        "Inline Document IR to differentiate instead of a session (stateless path).",
    },
    parameters: {
      type: "array" as const,
      items: { type: "string" as const },
      description:
        "Named parameters to differentiate. Omit to differentiate every parameter the document declares.",
    },
    quantities: {
      type: "array" as const,
      items: { type: "string" as const },
      description:
        "Quantities to differentiate: volume, mass, centroid_x|y|z, bbox_x|y|z. Defaults to volume and mass.",
    },
    part: {
      type: "number" as const,
      description:
        "Restrict to one solid part by index. Omit for whole-document quantities (volume and mass sum; the centroid is mass-weighted; the bbox is the union).",
    },
    density: {
      type: "number" as const,
      description:
        "Density fed to the mass integrals (mass = density · volume). Geometry is in mm, so pass g/cm^3 * 1e-3 for grams. Defaults to 1.",
    },
    find_trust_radius: {
      type: "boolean" as const,
      description:
        "Search for each parameter's topology trust radius by bisection (default true). Costs a bounded handful of extra document evaluations per parameter; turn it off for speed and every row falls back to the author's declared scrub bounds.",
    },
  },
  required: [],
};

export function sensitivity(input: unknown, engine: Engine): ToolResult {
  const args = (input ?? {}) as Record<string, unknown>;
  const { doc } = resolveDocInput(args);

  const request: SensitivityRequest = {};
  if (Array.isArray(args.parameters)) {
    request.parameters = args.parameters as string[];
  }
  if (Array.isArray(args.quantities)) {
    request.quantities = args.quantities as string[];
  }
  if (typeof args.part === "number") request.part = args.part as number;
  if (typeof args.density === "number") request.density = args.density as number;
  if (typeof args.find_trust_radius === "boolean") {
    request.findTrustRadius = args.find_trust_radius as boolean;
  }

  const report: SensitivityReport = engine.documentSensitivities(doc, request);

  return jsonResult({
    rendered: report.rendered,
    rows: report.table.rows,
    ranked: report.ranked,
    allUsable: report.allUsable,
    unusable: report.unusable,
    claims: report.claims,
  });
}

export const toolDefs: ToolDef[] = [
  {
    name: "list_parameters",
    pack: null,
    description:
      "List a document's named parameters: raw expression, resolved numeric value, unit, and scrub bounds (min/max). Pair with set_parameters to drive the design and parameter_gradient to differentiate it.",
    inputSchema: listParametersSchema,
    handler: (a) => listParameters(a),
    behavior: behavior({}),
  },
  {
    name: "set_parameters",
    pack: null,
    description:
      "Batch-update named parameter values on an open session document (e.g. { \"r\": 12, \"h\": 8 }). Every name must already be declared; values must be finite numbers. Returns a `changed` diff of the deltas and re-evaluates geometry integrity. When the document has design constraints, they re-solve automatically (constraint_solve in the result) — a parameter change relayouts everything bound to it. Undoable and persisted.",
    inputSchema: setParametersSchema,
    // Batch-updates named parameters → geometry changes, so it must
    // snapshot (undo) and persist like any other document mutator.
    handler: (a, c) => setParameters(a, c.engine),
    behavior: behavior({ writesDoc: true }),
  },
  {
    name: "parameter_gradient",
    pack: null,
    description:
      "Differentiate a document's QoIs with respect to a single named parameter via the differentiable seam: per solid part, returns d(volume)/d\u03b8, d(mass)/d\u03b8, d(centroid)/d\u03b8 (exact analytic seam derivatives) and d(bbox extents)/d\u03b8 (finite difference), alongside each QoI's value. The parameter must be bound onto some geometry field. \"Solve for the geometry\" starts here.",
    inputSchema: parameterGradientSchema,
    handler: (a, c) => parameterGradient(a, c.engine),
    behavior: behavior({}),
  },
  {
    name: "sensitivity",
    pack: null,
    description:
      "Which knob actually moves this number, and by how much. Differentiates a document's quantities (volume, mass, centroid, bbox) with respect to its named parameters and returns a ranked table: dJ/dθ with its unit, the route that produced it, and a **trust radius** — the interval of the parameter over which the derivative still describes the same solid, found by bisecting outward until the topology changes rather than assumed. Read `rendered` first. Rows ranked by influence (|dJ/dθ| × span), so the top row is the knob that commands the quantity. Volume/mass/centroid are exact seam derivatives; bbox extents are a max over vertices and come back as finite differences that can never claim `verified`. Use this instead of rebuilding the document N times to see what a change does.",
    inputSchema: sensitivitySchema,
    handler: (a, c) => sensitivity(a, c.engine),
    behavior: behavior({}),
  },
];
