/**
 * EM field-solver tools — the `vcad-kernel-em` crate over MCP.
 *
 * `simulate_em` solves one of three finite-volume problem classes
 * (axisymmetric magnetostatics, planar magnetostatics, electrostatics)
 * and extracts inductance / force / torque / capacitance with the
 * `vcad.em-claims/1` sets plus unified-receipt claims. Every claim
 * family carries a cross-route residual (two independent extraction
 * routes must agree). Predictions carry `basis: "predicted"` and roll up
 * Provisional — never verified — until the hardware is measured.
 */

import { behavior, type ToolDef } from "./tool-def.js";
import { getSession } from "./session-core.js";

const paramValue = {
  description:
    "A number, or the name of a named parameter bound via `parameters` " +
    "(magnetostatic classes only).",
  anyOf: [{ type: "number" as const }, { type: "string" as const }],
};

const rectRegion = {
  type: "object" as const,
  required: ["x_min_mm", "x_max_mm", "y_min_mm", "y_max_mm"],
  description:
    "Rectangular region (x = radius in the axisym class, y = z there).",
  properties: {
    x_min_mm: paramValue,
    x_max_mm: paramValue,
    y_min_mm: paramValue,
    y_max_mm: paramValue,
  },
};

const bcSchema = {
  type: "string" as const,
  enum: ["zero", "neumann"],
  description: "Outer boundary condition. Default zero (far-field).",
};

const electroShape = {
  type: "object" as const,
  required: ["type"],
  description:
    'Electrode/dielectric shape: `{"type":"rect",...}`, `{"type":"circle",' +
    '"cx_mm","cy_mm","radius_mm"}`, or `{"type":"circle_shell","cx_mm",' +
    '"cy_mm","r_inner_mm","r_outer_mm"}`. Literal numbers only.',
  properties: {
    type: {
      type: "string" as const,
      enum: ["rect", "circle", "circle_shell"],
    },
    x_min_mm: { type: "number" as const },
    x_max_mm: { type: "number" as const },
    y_min_mm: { type: "number" as const },
    y_max_mm: { type: "number" as const },
    cx_mm: { type: "number" as const },
    cy_mm: { type: "number" as const },
    radius_mm: { type: "number" as const },
    r_inner_mm: { type: "number" as const },
    r_outer_mm: { type: "number" as const },
  },
};

const emSpecSchema = {
  type: "object" as const,
  required: ["problem"],
  description:
    "Problem-tagged EM spec. `axisym_magnetostatics`: chamber `r_max_mm`/" +
    "`z_min_mm`/`z_max_mm` + `coils` [{region, turns, current_a}] + " +
    "`materials` [{region, mu_r, js_t?}] (js_t enables the saturable B-H " +
    "law) + `magnets` [{region, br_z_t, mu_r}] (axially magnetized rings: " +
    "speaker/actuator motors; named parameters allowed). `planar_magnetostatics`: `x_min_mm`/" +
    "`x_max_mm`/`y_min_mm`/`y_max_mm` + `conductors` [{region, " +
    "total_current_a}] + `magnets` [{region, br_x_t, br_y_t, mu_r}] + " +
    "`materials` + `periodic_x` (named parameters allowed). " +
    "`electrostatics`: `geometry` (\"axisymmetric\"|\"planar\") + domain " +
    "bounds + `electrodes` [{shape, potential_v}] (>= 2) + `dielectrics` " +
    "[{shape, eps_r}] — literal numbers only for this class.",
  properties: {
    problem: {
      type: "string" as const,
      enum: [
        "axisym_magnetostatics",
        "planar_magnetostatics",
        "electrostatics",
      ],
    },
    // axisym magnetostatics
    r_max_mm: paramValue,
    z_min_mm: paramValue,
    z_max_mm: paramValue,
    coils: {
      type: "array" as const,
      items: {
        type: "object" as const,
        required: ["region"],
        properties: {
          region: rectRegion,
          turns: { ...paramValue, description: "Winding turns. Default 1." },
          current_a: { ...paramValue, description: "Coil current, A." },
        },
      },
    },
    // planar magnetostatics
    x_min_mm: paramValue,
    x_max_mm: paramValue,
    y_min_mm: paramValue,
    y_max_mm: paramValue,
    conductors: {
      type: "array" as const,
      items: {
        type: "object" as const,
        required: ["region"],
        properties: {
          region: rectRegion,
          total_current_a: paramValue,
        },
      },
    },
    magnets: {
      type: "array" as const,
      items: {
        type: "object" as const,
        required: ["region"],
        properties: {
          region: rectRegion,
          br_x_t: { ...paramValue, description: "Remanence x, tesla." },
          br_y_t: { ...paramValue, description: "Remanence y, tesla." },
          br_z_t: {
            ...paramValue,
            description: "Axisym only: axial remanence, tesla (+ = north up).",
          },
          mu_r: { ...paramValue, description: "Recoil mu_r. Default 1." },
        },
      },
    },
    periodic_x: { type: "boolean" as const },
    // (axisym magnets share the `magnets` key: {region, br_z_t, mu_r})
    // shared magnetostatics
    materials: {
      type: "array" as const,
      items: {
        type: "object" as const,
        required: ["region", "mu_r"],
        properties: {
          region: rectRegion,
          mu_r: paramValue,
          js_t: {
            ...paramValue,
            description:
              "Saturation polarization, tesla — presence switches this " +
              "region to the nonlinear B-H law (Picard outer loop).",
          },
        },
      },
    },
    bc_r_outer: bcSchema,
    bc_z_low: bcSchema,
    bc_z_high: bcSchema,
    bc_x_low: bcSchema,
    bc_x_high: bcSchema,
    bc_y_low: bcSchema,
    bc_y_high: bcSchema,
    // electrostatics
    geometry: {
      type: "string" as const,
      enum: ["axisymmetric", "planar"],
    },
    electrodes: {
      type: "array" as const,
      items: {
        type: "object" as const,
        required: ["shape", "potential_v"],
        properties: {
          shape: electroShape,
          potential_v: { type: "number" as const },
        },
      },
    },
    dielectrics: {
      type: "array" as const,
      items: {
        type: "object" as const,
        required: ["shape", "eps_r"],
        properties: {
          shape: electroShape,
          eps_r: { type: "number" as const },
        },
      },
    },
  },
};

const magPartSchema = {
  type: "object" as const,
  required: ["part"],
  properties: {
    part: { type: "string" as const, description: "Part name or part id." },
    material: {
      type: "string" as const,
      description:
        "Material key for the magnetic table (default: the part's material): " +
        "steel-1008/1010/1018, low-carbon-steel, mild-steel, iron, " +
        "ferrite-y30/c8/y35, ndfeb-n35/n42/n52.",
    },
    role: {
      type: "string" as const,
      enum: ["steel", "magnet", "coil"],
      description: "Required when the material isn't in the table.",
    },
    mu_r: { ...paramValue, description: "Steel initial / magnet recoil mu_r." },
    js_t: { ...paramValue, description: "Steel saturation polarization, T." },
    linear: { type: "boolean" as const, description: "Drop the steel B-H law." },
    br_t: { ...paramValue, description: "Magnet remanence magnitude, T." },
    north: {
      type: "string" as const,
      enum: ["+axis", "-axis"],
      description: "Magnet polarity along the motor axis. Default +axis.",
    },
    turns: { ...paramValue, description: "Coil turns (required for coils)." },
    current_a: { ...paramValue, description: "Coil current, A. Default 0." },
  },
};

const magExtractSchema = {
  type: "object" as const,
  required: ["parts"],
  description:
    "Which parts of `document_id` form the axisymmetric magnetic device " +
    "(unselected parts are air). Every selected part must be a solid of " +
    "revolution about one common axis whose faces are all coaxial " +
    "cylinders or axis-perpendicular planes; its meridian section is " +
    "measured from the BRep into annuli (a T-yoke = two). Fail-closed by " +
    "part name: cones/fillets, off-axis holes, flats, unknown materials " +
    "without a role, overlapping parts, or a domain whose grid can't " +
    "resolve the thinnest gap.",
  properties: {
    parts: { type: "array" as const, minItems: 1, items: magPartSchema },
    domain: {
      type: "object" as const,
      properties: {
        pad_factor: {
          type: "number" as const,
          description: "Padding around the device as a multiple of its size. Default 1.",
        },
        r_max_mm: { type: "number" as const },
        z_min_mm: { type: "number" as const },
        z_max_mm: { type: "number" as const },
        cells_per_feature: {
          type: "number" as const,
          description: "Grid cells across the thinnest steel/magnet feature or gap. Default 4.",
        },
      },
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
    name: "simulate_em",
    pack: null,
    description:
      "Electromagnetic field solve (finite-volume SOR, the differentiable FEMM " +
      "replacement): axisymmetric magnetostatics (coil inductance, stored energy, axial " +
      "force — solenoids, coaxial coil pairs), planar magnetostatics (motor cross-section " +
      "torque and forces per meter of depth, permanent magnets, optional saturable B-H " +
      "iron), or electrostatics (two-terminal capacitance) — as data plus " +
      "`vcad.em-claims/1` sets and unified-receipt claims, each with a two-independent-" +
      "routes cross-check residual (energy vs linkage, JxB vs Maxwell stress, charge vs " +
      "energy). Predictions carry basis \"predicted\" and roll up Provisional until " +
      "measured. Static/2D only: no eddy currents or AC here, no 3D end effects, planar " +
      "results are per meter of depth. Deterministic. Cost ~O((nx*ny)^1.5) SOR sweeps; " +
      "the 81x81 default runs in well under a second — raise the grid for tight " +
      "tolerances.",
    inputSchema: {
      type: "object" as const,
      properties: {
        spec: {
          ...emSpecSchema,
          description: emSpecSchema.description + " Pass this OR `document_id` + `extract`.",
        },
        document_id: {
          type: "string" as const,
          description:
            "Session document to extract an axisym magnetostatic device from " +
            "(instead of `spec`): annuli measured from the document's revolved " +
            "parts. Requires `extract`. nx/ny default to the advised grid.",
        },
        extract: magExtractSchema,
        parameters: {
          type: "object" as const,
          additionalProperties: { type: "number" as const },
          description:
            "Bindings for named spec parameters (magnetostatic classes). With " +
            "`document_id` these override the document's parameter values.",
        },
        options: {
          type: "object" as const,
          properties: {
            nx: {
              type: "number" as const,
              description: "Grid nodes in x (r for axisym). Default 81.",
            },
            ny: {
              type: "number" as const,
              description: "Grid nodes in y (z for axisym). Default 81.",
            },
            tol: {
              type: "number" as const,
              description: "SOR relative tolerance. Default 1e-8.",
            },
            max_sweeps: {
              type: "number" as const,
              description: "SOR sweep cap. Default 200000.",
            },
            picard_max_iters: {
              type: "number" as const,
              description:
                "Saturable iron only: cap on the NONLINEAR outer loop over " +
                "the B-H law. Default 300. Separate from max_sweeps, which " +
                "caps the inner SOR solve and does nothing for a " +
                "non-converging material state. A deeply saturated motor " +
                "section contracts steadily but needs 150-250 iterations; " +
                "raise this when the error says the residual is still " +
                "falling.",
            },
            picard_tol: {
              type: "number" as const,
              description:
                "Saturable iron only: nonlinear convergence tolerance on the " +
                "largest relative reluctivity update. Default 1e-4. Raise it " +
                "to accept a looser material state deliberately.",
            },
            picard_relax: {
              type: "number" as const,
              description:
                "Saturable iron only: under-relaxation of the nonlinear " +
                "loop, in (0, 1]. Default 0.7. Lower it ONLY when the error " +
                "reports the residual has stopped falling (a limit cycle); " +
                "when the residual is still falling, lowering this makes " +
                "convergence strictly slower — raise picard_max_iters " +
                "instead.",
            },
            picard_adaptive: {
              type: "boolean" as const,
              description:
                "Saturable iron only: back the relaxation off automatically " +
                "when the nonlinear residual makes no progress over a " +
                "10-iteration window. Default FALSE — on a healthy saturated " +
                "solve it reacts to ordinary residual jitter and costs ~1.7x " +
                "the iterations. Turn it on for a device that is " +
                "demonstrably oscillating.",
            },
            drive_coil: {
              type: "number" as const,
              description:
                "Axisym: coil index the inductance claim is priced for. " +
                "Default 0.",
            },
            force_coil: {
              type: "number" as const,
              description:
                "Axisym: also emit force claims for this coil index.",
            },
            stress_probe: {
              type: "object" as const,
              description:
                "Axisym force cross-check: Maxwell-stress cylinder " +
                "{r_mm, z_lo_mm, z_hi_mm, panels?}.",
              properties: {
                r_mm: { type: "number" as const },
                z_lo_mm: { type: "number" as const },
                z_hi_mm: { type: "number" as const },
                panels: { type: "number" as const },
              },
            },
            torque: {
              type: "object" as const,
              description:
                "Planar (required for that class): torque center + mean " +
                "airgap radius + stack depth " +
                "{cx_mm, cy_mm, r_mean_m, depth_m, stress_line_y_mm?}.",
              properties: {
                cx_mm: { type: "number" as const },
                cy_mm: { type: "number" as const },
                r_mean_m: { type: "number" as const },
                depth_m: { type: "number" as const },
                stress_line_y_mm: { type: "number" as const },
              },
            },
            hot: {
              type: "number" as const,
              description:
                "Electrostatics: index of the driven electrode. Default 0.",
            },
            gap_probe: {
              type: "object" as const,
              description:
                "Axisym with magnets: air-gap region {r_inner_mm, r_outer_mm, " +
                "z_min_mm, z_max_mm} the `gap_flux_t` claim averages |B_r| over " +
                "(a Hall probe's reading). With magnets the inductance claim is " +
                "replaced by gap-flux + Bl (force factor of `drive_coil`) claims.",
              properties: {
                r_inner_mm: { type: "number" as const },
                r_outer_mm: { type: "number" as const },
                z_min_mm: { type: "number" as const },
                z_max_mm: { type: "number" as const },
              },
            },
          },
        },
      },
    },
    handler: (args, ctx) => {
      const a = args as Json;
      const params = (a.parameters ?? {}) as Record<string, number>;
      if (a.document_id === undefined) {
        if (a.spec === undefined) {
          throw new Error("simulate_em: pass `spec`, or `document_id` + `extract`");
        }
        return textResult(
          ctx.engine.emSimulate(
            JSON.stringify(a.spec),
            JSON.stringify(params),
            JSON.stringify(a.options ?? {}),
          ),
        );
      }
      if (a.spec !== undefined) {
        throw new Error("simulate_em: pass `spec` OR `document_id`, not both");
      }
      if (a.extract === undefined) {
        throw new Error(
          "simulate_em: `document_id` requires `extract` (steel/magnet/coil parts)",
        );
      }
      const documentId = String(a.document_id);
      const ex = ctx.engine.extractAxisymMagSpec(
        JSON.stringify(getSession(documentId)),
        JSON.stringify(a.extract),
      ) as {
        spec: Json;
        parameters: Record<string, number>;
        axis: unknown;
        parts: unknown[];
        grid: { nx: number; ny: number; dx_mm: number; thinnest_feature_mm: number };
      };
      const parameters = { ...ex.parameters, ...params };
      const options = {
        nx: ex.grid.nx,
        ny: ex.grid.ny,
        ...((a.options ?? {}) as Json),
      };
      const spec = { problem: "axisym_magnetostatics", ...ex.spec };
      const out = ctx.engine.emSimulate(
        JSON.stringify(spec),
        JSON.stringify(parameters),
        JSON.stringify(options),
      ) as Json;
      return textResult({
        ...out,
        extracted: {
          document_id: documentId,
          spec,
          parameters,
          axis: ex.axis,
          parts: ex.parts,
          grid: ex.grid,
        },
      });
    },
    behavior: behavior({}),
  },
];
