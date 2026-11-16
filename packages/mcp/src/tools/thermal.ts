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
import { getSession } from "./session-core.js";

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
    '`{"type":"FixedTemperature","temperature_c":..}`, or ' +
    '`{"type":"Convection","h_w_m2k":..,"ambient_c":..}`.',
  properties: {
    type: {
      type: "string" as const,
      enum: ["Adiabatic", "FixedTemperature", "Convection"],
    },
    temperature_c: paramValue,
    h_w_m2k: paramValue,
    ambient_c: paramValue,
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
              "face's temperature or a Convection face's ambient. " +
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

const studyBoundarySchema = {
  type: "object" as const,
  required: ["type"],
  description:
    'Boundary: `{"type":"adiabatic"}`, `{"type":"convection","hWM2k":..,' +
    '"ambientC":..}`, or `{"type":"fixedTemperature","temperatureC":..}`. ' +
    "Numbers may be document parameter names.",
  properties: {
    type: {
      type: "string" as const,
      enum: ["adiabatic", "convection", "fixedTemperature"],
    },
    hWM2k: paramValue,
    ambientC: paramValue,
    temperatureC: paramValue,
  },
};

const studyRegionSchema = {
  type: "object" as const,
  required: ["min", "max"],
  description: "World-frame AABB sub-region of the part, mm.",
  properties: {
    min: { type: "array" as const, minItems: 3, maxItems: 3, items: { type: "number" as const } },
    max: { type: "array" as const, minItems: 3, maxItems: 3, items: { type: "number" as const } },
  },
};

const thermalStudySchema = {
  type: "object" as const,
  required: ["parts"],
  description:
    "A thermal study over the document's own parts (the same shape as a " +
    "`thermal` entry in `document.analysis_studies`). The listed parts are " +
    "voxelized from their tessellations onto a grid spanning their union " +
    "bounding box; each voxel takes the material of the part filling most " +
    "of it (solid when >= half full). Sources and sinks name parts; numbers " +
    "may be document parameter names (e.g. a `defparam amp_loss_w`). " +
    "Fail-closed, naming the part: a part thinner than `minVoxelsAcross` " +
    "voxels (default 2) along any axis, overlapping parts, an open mesh, a " +
    "material with no thermal properties and no override, a source box that " +
    "would also cover another part.",
  properties: {
    parts: {
      type: "array" as const,
      minItems: 1,
      items: { type: "string" as const },
      description: "Parts to voxelize (name or part id). Unlisted parts are void.",
    },
    divisions: {
      type: "array" as const,
      minItems: 3,
      maxItems: 3,
      items: { type: "number" as const },
      description: "Voxels per axis. Give this OR `pitchMm`.",
    },
    pitchMm: {
      type: "array" as const,
      minItems: 3,
      maxItems: 3,
      items: { type: "number" as const },
      description: "Target voxel pitch per axis, mm (rounded down to span the domain exactly).",
    },
    paddingMm: { type: "number" as const, description: "Void margin around the parts, mm. Default 0." },
    minVoxelsAcross: { type: "number" as const, description: "Thin-wall guard, voxels. Default 2." },
    materials: {
      type: "array" as const,
      description:
        "Per-part overrides of the built-in table (keyed by the part's material " +
        "string: pla, petg, abs, nylon, fr4, aluminium, copper, steel, " +
        "stainless, brass, silicon, glass, air, ...). Give `kWMk` (1 or 3 world-" +
        "axis values) OR `laminate` (through-plane = the part's thinnest axis). " +
        "A real PCB with copper planes needs `laminate` (~15 in-plane / ~0.4 " +
        "through) — the table's fr4 is bare laminate.",
      items: {
        type: "object" as const,
        required: ["part"],
        properties: {
          part: { type: "string" as const },
          kWMk: { type: "array" as const, minItems: 1, maxItems: 3, items: paramValue },
          laminate: {
            type: "object" as const,
            required: ["inPlaneWMk", "throughPlaneWMk"],
            properties: { inPlaneWMk: paramValue, throughPlaneWMk: paramValue },
          },
          heatCapacityJM3k: paramValue,
        },
      },
    },
    sources: {
      type: "array" as const,
      items: {
        type: "object" as const,
        required: ["name", "part", "powerW"],
        properties: {
          name: { type: "string" as const },
          part: { type: "string" as const },
          powerW: paramValue,
          region: studyRegionSchema,
        },
      },
    },
    sinks: {
      type: "array" as const,
      description: "Heat sinks: parts (or sub-regions) held at a fixed temperature.",
      items: {
        type: "object" as const,
        required: ["part", "temperatureC"],
        properties: {
          part: { type: "string" as const },
          temperatureC: paramValue,
          region: studyRegionSchema,
        },
      },
    },
    exposed: {
      ...studyBoundarySchema,
      description: "Boundary on every solid face touching void. Default adiabatic.",
    },
    domainFaces: {
      type: "array" as const,
      minItems: 6,
      maxItems: 6,
      items: studyBoundarySchema,
      description: "Per domain face `[-x,+x,-y,+y,-z,+z]`; omitted = all take `exposed`.",
    },
    referenceC: paramValue,
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
      "and the run-integrated energy audit. Conduction only: convection enters as film " +
      "coefficients you supply (h values are the dominant uncertainty), no radiation, " +
      "no fluid flow. Full temperature fields are not returned (summaries + claims are). " +
      "Cost scales with voxel count (capped at 2M; transient runs also capped at 20k " +
      "steps and 1e9 voxel-steps); a 20x20x2 board solves instantly, 100x100x13 in " +
      "seconds. Instead of a hand-written `spec`, pass `document_id` to solve the " +
      "document's own parts: its thermal analysis study (`study_id`, or the only " +
      "one) or an inline `study` voxelizes the selected parts from their " +
      "tessellations, ties sources/sinks to parts, and reads named document " +
      "parameters (steady only).",
    inputSchema: {
      type: "object" as const,
      properties: {
        spec: {
          ...thermalSpecSchema,
          description:
            thermalSpecSchema.description + " Pass this OR `document_id`.",
        },
        document_id: {
          type: "string" as const,
          description:
            "Session document to solve (instead of `spec`): the study's parts are " +
            "voxelized from the document's own evaluated geometry.",
        },
        study_id: {
          type: "string" as const,
          description:
            "With `document_id`: id of the thermal study in " +
            "`document.analysis_studies` (default: the only one).",
        },
        study: thermalStudySchema,
        transient: transientSchema,
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
            tol: {
              type: "number" as const,
              description: "PCG relative tolerance. Default 1e-8.",
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
      if (a.document_id !== undefined) {
        if (a.spec !== undefined) {
          throw new Error("solve_thermal: pass `spec` OR `document_id`, not both");
        }
        if (a.transient !== undefined) {
          throw new Error(
            "solve_thermal: `transient` is not supported with `document_id` yet — steady only",
          );
        }
        const documentId = String(a.document_id);
        const doc = getSession(documentId);
        const out = ctx.engine.thermalSolveDocument(
          JSON.stringify(doc),
          a.study === undefined ? "" : JSON.stringify(a.study),
          a.study_id === undefined ? "" : String(a.study_id),
          JSON.stringify(a.parameters ?? {}),
          JSON.stringify(a.options ?? {}),
        ) as Json;
        return textResult({
          ...out,
          extracted: { document_id: documentId, ...(out.extracted as Json) },
        });
      }
      if (a.spec === undefined) {
        throw new Error("solve_thermal: pass `spec`, or `document_id` (+ `study` / `study_id`)");
      }
      if (a.study !== undefined || a.study_id !== undefined) {
        throw new Error("solve_thermal: `study` / `study_id` need `document_id`");
      }
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
