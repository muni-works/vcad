/**
 * Tolerance stackup tools — the `vcad-kernel-tolerance` crate over MCP.
 *
 * `analyze_tolerance_stackup` runs worst-case, RSS, and seeded Monte
 * Carlo over a linear assembly chain, computes exact sensitivities, and
 * returns the `vcad.tolerance-claims/1` set plus unified-receipt claims.
 * Predictions carry `basis: "predicted"` and roll up Provisional — never
 * verified — until the parts are measured.
 */

import { behavior, type ToolDef } from "./tool-def.js";
import { getSession } from "./session-core.js";

const paramValue = {
  description:
    "A number, or the name of a named parameter bound via `parameters`.",
  anyOf: [{ type: "number" as const }, { type: "string" as const }],
};

const distSchema = {
  type: "object" as const,
  description:
    "Contributor distribution. `normal_from_tol` derives sigma from the " +
    "(symmetric) tolerance band via the convention; `uniform`/`two_point` " +
    "must lie within the drawing limits.",
  required: ["type"],
  properties: {
    type: {
      type: "string" as const,
      enum: ["normal", "normal_from_tol", "uniform", "two_point"],
    },
    mean: { ...paramValue, description: "normal: mean offset, mm." },
    sigma: { ...paramValue, description: "normal: one sigma, mm." },
    convention: {
      type: "object" as const,
      description:
        'normal_from_tol: `{"type":"three_sigma"}` or `{"type":"k_sigma","k":4}`.',
      properties: {
        type: {
          type: "string" as const,
          enum: ["three_sigma", "k_sigma"],
        },
        k: { type: "number" as const },
      },
    },
    lo: { ...paramValue, description: "uniform: lower offset, mm." },
    hi: { ...paramValue, description: "uniform: upper offset, mm." },
    a: { ...paramValue, description: "two_point: first offset, mm." },
    b: { ...paramValue, description: "two_point: second offset, mm." },
    p_b: { ...paramValue, description: "two_point: probability of b." },
  },
};

const stackupSpecSchema = {
  type: "object" as const,
  required: ["name", "contributors", "requirement"],
  description:
    "Linear tolerance stackup: gap = sum(coeff_i * x_i) over contributor " +
    "dimensions, graded against a fit requirement. Units mm. Any numeric " +
    "field may instead be a string naming a parameter supplied in " +
    "`parameters` (fail-closed: unbound names error).",
  properties: {
    name: { type: "string" as const },
    contributors: {
      type: "array" as const,
      minItems: 1,
      items: {
        type: "object" as const,
        required: [
          "name",
          "coeff",
          "nominal",
          "tol_minus",
          "tol_plus",
          "dist",
        ],
        properties: {
          name: { type: "string" as const },
          coeff: {
            ...paramValue,
            description:
              "Direction coefficient (+1 adds to the gap, -1 subtracts).",
          },
          nominal: { ...paramValue, description: "Nominal dimension, mm." },
          tol_minus: {
            ...paramValue,
            description: "Lower tolerance magnitude, mm (>= 0).",
          },
          tol_plus: {
            ...paramValue,
            description: "Upper tolerance magnitude, mm (>= 0).",
          },
          dist: distSchema,
        },
      },
    },
    requirement: {
      type: "object" as const,
      required: ["name"],
      description:
        "Fit requirement on the closing gap; at least one bound required.",
      properties: {
        name: { type: "string" as const },
        lower_mm: paramValue,
        upper_mm: paramValue,
      },
    },
  },
};

const stackupExtractSchema = {
  type: "object" as const,
  required: ["clearance", "feature"],
  description:
    "Derive the stack from `document_id`: the fit is the document's labeled " +
    "clearance assertion (`clearance_specs`), measured analytically from the " +
    "BRep (radial: coaxial bore/shaft, gap = r_bore − r_shaft, negative = " +
    "interference). Lower bound = the assertion's min_mm; a press fit needs " +
    "`upper_mm` here (ClearanceSpec has no upper bound, and its receipt check " +
    "reads an interference fit as Violated). Chain coefficients are " +
    "d(gap)/d(parameter) by re-evaluating the document at p ± tolerance; " +
    "parameters that don't move the gap drop out. Tolerances come from the " +
    "document parameters (`tolerance: {plus, minus, sigmaK?}`, loon `:tol`). " +
    "Fail-closed: a parameter that moves the gap with no tolerance (and no " +
    "`process_tolerance_mm`), a gap nonlinear in a parameter, or an " +
    "ambiguous/absent fit errors.",
  properties: {
    clearance: {
      type: "string" as const,
      description: "Label of the clearance assertion naming the two part groups.",
    },
    feature: {
      type: "string" as const,
      enum: ["radial"],
      description: "Fit feature to measure between the groups (radial bore/shaft only, for now).",
    },
    fit_window_mm: {
      type: "number" as const,
      description: "Only fits with |gap| <= this are candidates. Default 1.0.",
    },
    lower_mm: {
      type: "number" as const,
      description: "Lower gap bound (overrides the assertion's min_mm).",
    },
    upper_mm: {
      type: "number" as const,
      description:
        "Upper gap bound, mm — a press/transition fit needs one (e.g. -0.01: " +
        "less interference than this rattles).",
    },
    process_tolerance_mm: {
      type: "number" as const,
      description:
        "Symmetric 3-sigma process tolerance (e.g. 0.1 for FDM) applied to " +
        "gap-driving parameters that declare none.",
    },
    parameters: {
      type: "array" as const,
      items: { type: "string" as const },
      description: "Restrict candidate parameters (default: all literal ones).",
    },
  },
};

type Json = Record<string, unknown>;

function textResult(payload: unknown) {
  return {
    content: [
      {
        type: "text" as const,
        text: JSON.stringify(payload, null, 2),
      },
    ],
  };
}

export const toolDefs: ToolDef[] = [
  {
    name: "analyze_tolerance_stackup",
    pack: null,
    description:
      "Dimensional tolerance stackup analysis over a linear assembly chain: worst-case " +
      "extremes, RSS statistics (yield, Cp/Cpk), seeded Monte Carlo fit probability with " +
      "batch error bars, and exact per-contributor sensitivities (variance share, " +
      "d yield/d nominal) — as data plus `vcad.tolerance-claims/1` and unified-receipt " +
      "claims. Predictions carry basis \"predicted\" and roll up Provisional until parts " +
      "are measured. Linear 1D chains only: no 3D kinematic loops here (project those to " +
      "a chain first), distributions are the declared ones (assumed 3-sigma unless " +
      "stated). Deterministic for a given seed. Cost is O(n samples); the 100k default " +
      "runs in well under a second.",
    inputSchema: {
      type: "object" as const,
      properties: {
        spec: {
          ...stackupSpecSchema,
          description:
            stackupSpecSchema.description + " Pass this OR `document_id` + `extract`.",
        },
        document_id: {
          type: "string" as const,
          description:
            "Session document to derive the stack from (instead of `spec`). " +
            "Requires `extract`.",
        },
        extract: stackupExtractSchema,
        parameters: {
          type: "object" as const,
          additionalProperties: { type: "number" as const },
          description:
            "Bindings for named spec parameters, `{name: value}`. With " +
            "`document_id` these override the document's parameter values.",
        },
        options: {
          type: "object" as const,
          properties: {
            n: {
              type: "number" as const,
              description: "Monte Carlo samples. Default 100000 (min 100).",
            },
            seed: {
              type: "number" as const,
              description: "MC seed (deterministic). Default 0x5EED7015.",
            },
            batches: {
              type: "number" as const,
              description: "MC error-bar batches. Default 16.",
            },
          },
        },
      },
    },
    handler: (args, ctx) => {
      const a = args as Json;
      const params = (a.parameters ?? {}) as Record<string, number>;
      const options = JSON.stringify(a.options ?? {});
      if (a.document_id === undefined) {
        if (a.spec === undefined) {
          throw new Error(
            "analyze_tolerance_stackup: pass `spec`, or `document_id` + `extract`",
          );
        }
        return textResult(
          ctx.engine.toleranceAnalyze(JSON.stringify(a.spec), JSON.stringify(params), options),
        );
      }
      if (a.spec !== undefined) {
        throw new Error("analyze_tolerance_stackup: pass `spec` OR `document_id`, not both");
      }
      if (a.extract === undefined) {
        throw new Error(
          "analyze_tolerance_stackup: `document_id` requires `extract` (clearance label + feature)",
        );
      }
      const documentId = String(a.document_id);
      const ex = ctx.engine.extractStackupSpec(
        JSON.stringify(getSession(documentId)),
        JSON.stringify(a.extract),
      ) as {
        spec: unknown;
        parameters: Record<string, number>;
        gap_mm: number;
        fit_parts: string[];
        contributors: unknown[];
        independent: string[];
        fixed_geometry_mm: number;
      };
      const parameters = { ...ex.parameters, ...params };
      const out = ctx.engine.toleranceAnalyze(
        JSON.stringify(ex.spec),
        JSON.stringify(parameters),
        options,
      ) as Json;
      return textResult({
        ...out,
        extracted: {
          document_id: documentId,
          spec: ex.spec,
          parameters,
          gap_mm: ex.gap_mm,
          fit_parts: ex.fit_parts,
          contributors: ex.contributors,
          independent: ex.independent,
          fixed_geometry_mm: ex.fixed_geometry_mm,
        },
      });
    },
    behavior: behavior({}),
  },
];
