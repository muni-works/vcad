/**
 * Thermal FEA tools — the `vcad-kernel-thermal` crate over MCP.
 *
 * `solve_thermal` runs a steady heat-conduction solve on a voxel grid
 * (harmonic-mean finite volumes + PCG) and returns the temperature
 * summary, per-source theta (junction-to-ambient), the energy-balance
 * audit, and the `vcad.thermal-claims/1` set plus unified-receipt claims.
 * Predictions carry `basis: "predicted"` and roll up Provisional — never
 * verified — until the hardware is measured.
 */

import { behavior, type ToolDef } from "./tool-def.js";

const paramValue = {
  description:
    "A number, or the name of a named parameter bound via `parameters`.",
  anyOf: [{ type: "number" as const }, { type: "string" as const }],
};

const paramVec3 = {
  type: "array" as const,
  minItems: 3,
  maxItems: 3,
  items: paramValue,
};

const shapeSchema = {
  type: "object" as const,
  required: ["type"],
  description:
    'Region shape: `{"type":"Box","min_mm":[..],"size_mm":[..]}` or ' +
    '`{"type":"Tube","axis":"Z","center_mm":[..2],"span_mm":[..2],' +
    '"outer_radius_mm":..,"inner_radius_mm":..}`.',
  properties: {
    type: { type: "string" as const, enum: ["Box", "Tube"] },
    min_mm: paramVec3,
    size_mm: paramVec3,
    axis: { type: "string" as const, enum: ["X", "Y", "Z"] },
    center_mm: {
      type: "array" as const,
      minItems: 2,
      maxItems: 2,
      items: paramValue,
    },
    span_mm: {
      type: "array" as const,
      minItems: 2,
      maxItems: 2,
      items: paramValue,
    },
    outer_radius_mm: paramValue,
    inner_radius_mm: paramValue,
  },
};

const boundarySchema = {
  type: "object" as const,
  required: ["type"],
  description:
    'Boundary condition: `{"type":"Adiabatic"}`, ' +
    '`{"type":"FixedTemperature","temperature_c":..}`, ' +
    '`{"type":"Convection","h_w_m2k":..,"ambient_c":..}`, or ' +
    '`{"type":"Radiation","emissivity":..,"ambient_c":..}` (gray-diffuse ' +
    "eps*sigma*(Ts^4-Tamb^4), optional parallel `h_w_m2k` (0 in vacuum), " +
    "optional `area_scale` = true/voxel surface area (a voxel staircase " +
    "over-counts curved area ~4/pi for cylinders, ~1.5 for spheres, which " +
    "over-cools: the result reports `voxel_area_m2` per slot so you can " +
    "compute it), optional `enclosure` {emissivity, area_m2} for a " +
    "two-surface gray enclosure such as a grid inside a vacuum chamber " +
    "whose wall temperature is `ambient_c`). Any radiation face makes the " +
    "solve nonlinear (Newton); temperatures must be degC.",
  properties: {
    type: {
      type: "string" as const,
      enum: ["Adiabatic", "FixedTemperature", "Convection", "Radiation"],
    },
    temperature_c: paramValue,
    h_w_m2k: paramValue,
    ambient_c: paramValue,
    emissivity: {
      ...paramValue,
      description: "Radiation: surface emissivity in (0, 1]. Required.",
    },
    area_scale: {
      ...paramValue,
      description: "Radiation: radiating-area multiplier (> 0), default 1.",
    },
    enclosure: {
      type: "object" as const,
      required: ["emissivity", "area_m2"],
      description:
        "Radiation: enclosing wall (view factor 1). eps_eff = " +
        "1/(1/eps + (A_s/A_w)(1/eps_w - 1)); A_w must exceed the slot's " +
        "radiating area.",
      properties: {
        emissivity: paramValue,
        area_m2: paramValue,
      },
    },
  },
};

const thermalSpecSchema = {
  type: "object" as const,
  required: ["origin_mm", "size_mm", "divisions", "materials"],
  description:
    "Voxelized conduction domain: an axis-aligned box discretized into " +
    "`divisions` voxels, with material regions (per-axis k), power " +
    "sources, optional fixed-temperature regions, and per-face boundary " +
    "conditions `[-x,+x,-y,+y,-z,+z]`. Units mm / W / degC. Any numeric " +
    "field (except `divisions`) may instead be a string naming a " +
    "parameter supplied in `parameters` (fail-closed: unbound names " +
    "error).",
  properties: {
    origin_mm: paramVec3,
    size_mm: paramVec3,
    divisions: {
      type: "array" as const,
      minItems: 3,
      maxItems: 3,
      items: { type: "number" as const },
      description: "Voxel counts per axis (plain integers, the cost knob).",
    },
    materials: {
      type: "array" as const,
      minItems: 1,
      items: {
        type: "object" as const,
        required: ["shape", "k_w_mk"],
        properties: {
          shape: shapeSchema,
          k_w_mk: {
            ...paramVec3,
            description: "Per-axis conductivity, W/(m*K).",
          },
        },
      },
    },
    sources: {
      type: "array" as const,
      items: {
        type: "object" as const,
        required: ["name", "shape", "power_w"],
        properties: {
          name: { type: "string" as const },
          shape: shapeSchema,
          power_w: paramValue,
        },
      },
    },
    fixed: {
      type: "array" as const,
      items: {
        type: "object" as const,
        required: ["shape", "temperature_c"],
        properties: {
          shape: shapeSchema,
          temperature_c: paramValue,
        },
      },
    },
    domain_faces: {
      type: "array" as const,
      minItems: 6,
      maxItems: 6,
      items: boundarySchema,
      description:
        "Boundary per domain face `[-x,+x,-y,+y,-z,+z]`. Default all " +
        "adiabatic — ground the system with at least one non-adiabatic " +
        "face or fixed region.",
    },
    exposed: {
      ...boundarySchema,
      description:
        "Boundary applied to interior void-facing surfaces. Default adiabatic.",
    },
    reference_c: {
      ...paramValue,
      description:
        "Explicit theta reference temperature; usually inferred from the " +
        "single ambient.",
    },
  },
};

const transientSchema = {
  type: "object" as const,
  required: ["initial_c", "segments"],
  description:
    "Transient mode: backward-Euler time stepping from a uniform initial " +
    "temperature over a schedule of piecewise-constant segments (an RTP " +
    "ramp/soak/cool recipe, an ambient step, a duty cycle). Every " +
    "material must declare `heat_capacity_j_m3k` (rho*c_p, J/(m^3*K)). " +
    "Returns T_max and per-source time series instead of a single steady " +
    "state, plus the energy audit integrated over the run.",
  properties: {
    initial_c: {
      ...paramValue,
      description: "Uniform initial temperature of the free field, degC.",
    },
    segments: {
      type: "array" as const,
      minItems: 1,
      items: {
        type: "object" as const,
        required: ["duration_s", "dt_s"],
        properties: {
          duration_s: {
            ...paramValue,
            description: "Segment duration, s.",
          },
          dt_s: {
            ...paramValue,
            description:
              "Target time step, s. Implicit stepping is stable at any " +
              "dt; pick it to resolve the fastest time constant you care " +
              "about. The realized step divides the duration exactly.",
          },
          source_power_w: {
            type: "object" as const,
            additionalProperties: paramValue,
            description:
              "Source-power overrides for this segment by source name, W " +
              "(fail-closed on unknown names). Unnamed sources keep the " +
              "spec's power.",
          },
          face_temperature_c: {
            type: "object" as const,
            additionalProperties: paramValue,
            description:
              'Boundary-temperature overrides by face label ("-x","+x",' +
              '"-y","+y","-z","+z","exposed"): retunes a FixedTemperature ' +
              "face's temperature or a Convection/Radiation face's ambient. " +
              "Overriding an adiabatic face errors (fail-closed).",
          },
          fixed_temperature_c: {
            type: "object" as const,
            additionalProperties: paramValue,
            description:
              "Fixed-region temperature overrides keyed by the region's " +
              "index in `fixed` (as a string).",
          },
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
    name: "solve_thermal",
    pack: null,
    description:
      "Heat-conduction FEA on a voxel grid: harmonic-mean finite volumes + PCG, " +
      "returning T_max and its location, per-source theta_ja (K/W, junction-to-ambient), " +
      "reservoir loads, and an energy-balance audit — as data plus " +
      "`vcad.thermal-claims/1` and unified-receipt claims. Predictions carry basis " +
      "\"predicted\" and roll up Provisional until hardware is measured. Steady state by " +
      "default; pass `transient` for backward-Euler time stepping over a piecewise-" +
      "constant drive schedule (RTP ramp/soak/cool, ambient steps, duty cycles — needs " +
      "per-material `heat_capacity_j_m3k`), which returns T_max/per-source time series " +
      "and the run-integrated energy audit. Conduction plus optional gray-diffuse surface " +
      "radiation (`Radiation` boundary: emissivity you supply, radiating to an ambient or " +
      "an enclosing wall; solved by Newton; reports per-slot radiated W and a " +
      "`radiation_fraction` claim). Convection enters as film coefficients you supply " +
      "(h and emissivity are the dominant uncertainties); no surface-to-surface exchange " +
      "between parts of the model, no fluid flow. Full temperature fields are not " +
      "returned (summaries + claims are). " +
      "Cost scales with voxel count (capped at 2M; transient runs also capped at 20k " +
      "steps and 1e9 voxel-steps); a 20x20x2 board solves instantly, 100x100x13 in " +
      "seconds.",
    inputSchema: {
      type: "object" as const,
      required: ["spec"],
      properties: {
        spec: thermalSpecSchema,
        transient: transientSchema,
        parameters: {
          type: "object" as const,
          additionalProperties: { type: "number" as const },
          description: "Bindings for named spec parameters, `{name: value}`.",
        },
        options: {
          type: "object" as const,
          properties: {
            tol: {
              type: "number" as const,
              description:
                "Relative tolerance (PCG, and the Newton residual when a " +
                "face radiates). Default 1e-8.",
            },
            max_iters: {
              type: "number" as const,
              description: "PCG iteration cap. Default 50000.",
            },
          },
        },
      },
    },
    handler: (args, ctx) => {
      const a = args as Json;
      const out = a.transient
        ? ctx.engine.thermalSolveTransient(
            JSON.stringify(a.spec),
            JSON.stringify(a.transient),
            JSON.stringify(a.parameters ?? {}),
            JSON.stringify(a.options ?? {}),
          )
        : ctx.engine.thermalSolve(
            JSON.stringify(a.spec),
            JSON.stringify(a.parameters ?? {}),
            JSON.stringify(a.options ?? {}),
          );
      return textResult(out);
    },
    behavior: behavior({}),
  },
];
