/**
 * Charged-particle optics tools — the `vcad-kernel-particle` crate over MCP.
 *
 * `simulate_charged_particles` solves an axisymmetric electrode device
 * (fusor / shielded-grid IEC / ring-trap family), traces a deuteron
 * ensemble, and returns figures of merit plus the `vcad.particle-claims/1`
 * set and unified-receipt claims. Predictions carry `basis: "predicted"`
 * and roll up Provisional — never verified — until bound to bench
 * measurements (see `docs/shielded-grid-experiment.md`).
 *
 * `optimize_electrodes` runs multi-start gradient ascent over named spec
 * parameters against predicted D-D yield per ion — FD by default, or a
 * hybrid (adjoint for potentials / ampere-turns, FD for geometry). Multi-start is the
 * default because the yield landscape is measurably multimodal
 * (recirculation hill vs energy-quality hill — `docs/particle-optics-m0.md`).
 */

import { behavior, type ToolDef } from "./tool-def.js";
import { getSession } from "./session-core.js";

const paramValue = {
  description:
    "A number, or the name of a named parameter bound via `parameters`.",
  anyOf: [{ type: "number" as const }, { type: "string" as const }],
};

const deviceSpecSchema = {
  type: "object" as const,
  required: ["chamber_radius_mm", "chamber_half_height_mm", "rings"],
  description:
    "Axisymmetric device: grounded cylindrical chamber + biased (optionally " +
    "current-carrying) wire rings, coaxial with Z. Units mm / volts / " +
    "ampere-turns. Any numeric field may instead be a string naming a " +
    "parameter supplied in `parameters` (fail-closed: unbound names error).",
  properties: {
    chamber_radius_mm: paramValue,
    chamber_half_height_mm: paramValue,
    wall_potential_v: {
      ...paramValue,
      description: "Chamber wall potential, volts. Default 0 (grounded).",
    },
    rings: {
      type: "array" as const,
      minItems: 1,
      items: {
        type: "object" as const,
        required: ["ring_radius_mm", "z_mm", "wire_radius_mm", "potential_v"],
        properties: {
          ring_radius_mm: paramValue,
          z_mm: paramValue,
          wire_radius_mm: paramValue,
          potential_v: paramValue,
          ampere_turns: {
            ...paramValue,
            description:
              "Circulating current, ampere-turns (+ = CCW viewed from +Z). " +
              "Default 0. This is the magnetic grid-shielding knob.",
          },
        },
      },
    },
  },
};

const vec3 = {
  type: "array" as const,
  minItems: 3,
  maxItems: 3,
  items: paramValue,
};

const wireCommon = {
  wire_radius_mm: paramValue,
  potential_v: paramValue,
};

const deviceSpec3dSchema = {
  type: "object" as const,
  required: ["chamber", "electrodes"],
  description:
    "Non-axisymmetric (3D) device: thin-wire electrodes along arbitrary paths " +
    "inside a grounded sphere — real fusor globe grids (great circles, " +
    "latitude + meridian), stalks, rings. Solved by the thin-wire boundary-" +
    "element method; refused (fail-closed) if the wire model's surface-" +
    "residual gates fail. Units mm / volts. Unknown fields are errors. Any " +
    "numeric field may be a parameter name bound in `parameters`.",
  properties: {
    chamber: {
      type: "object" as const,
      required: ["kind"],
      description:
        '`{"kind":"sphere","radius_mm":…}` (grounded sphere about the origin) ' +
        'or `{"kind":"unbounded"}`.',
      properties: {
        kind: { type: "string" as const, enum: ["sphere", "unbounded"] },
        radius_mm: paramValue,
      },
    },
    wall_potential_v: paramValue,
    electrodes: {
      type: "array" as const,
      minItems: 1,
      items: {
        type: "object" as const,
        required: ["kind", "wire_radius_mm", "potential_v"],
        description:
          "One of: great_circle_globe {grid_radius_mm, layout " +
          '("orthogonal3"|"cuboctahedral4"|"icosidodecahedral6") | normals: [[x,y,z],…]}; ' +
          "lat_long_globe {grid_radius_mm, n_lat, n_long}; circle {center_mm, normal, " +
          "radius_mm, ampere_turns?}; arc {center_mm, normal, start_mm, sweep_deg}; " +
          "polyline {points_mm, closed?}; ring {ring_radius_mm, z_mm, ampere_turns?}. " +
          "All take wire_radius_mm and potential_v.",
        properties: {
          kind: {
            type: "string" as const,
            enum: [
              "great_circle_globe",
              "lat_long_globe",
              "circle",
              "arc",
              "polyline",
              "ring",
            ],
          },
          ...wireCommon,
          grid_radius_mm: paramValue,
          layout: {
            type: "string" as const,
            enum: ["orthogonal3", "cuboctahedral4", "icosidodecahedral6"],
          },
          normals: { type: "array" as const, items: vec3 },
          n_lat: { type: "number" as const },
          n_long: { type: "number" as const },
          center_mm: vec3,
          normal: vec3,
          start_mm: vec3,
          radius_mm: paramValue,
          sweep_deg: paramValue,
          points_mm: { type: "array" as const, items: vec3 },
          closed: { type: "boolean" as const },
          ring_radius_mm: paramValue,
          z_mm: paramValue,
          ampere_turns: paramValue,
        },
      },
    },
  },
};

const parametersSchema = {
  type: "object" as const,
  additionalProperties: { type: "number" as const },
  description: "Bindings for named spec parameters, `{name: value}`.",
};

const simOptionsSchema = {
  type: "object" as const,
  properties: {
    nr: {
      type: "number" as const,
      description: "Radial grid nodes. Default 101.",
    },
    nz: {
      type: "number" as const,
      description: "Axial grid nodes. Default 201.",
    },
    particles: {
      type: "number" as const,
      description: "Deuteron ensemble size. Default 64.",
    },
    max_passes: {
      type: "number" as const,
      description: "Core-pass cap (censoring boundary). Default 25.",
    },
    ion_current_a: {
      type: "number" as const,
      description: "Operating-point ion current, A. Default 0.010.",
    },
    d2_pressure_mtorr: {
      type: "number" as const,
      description: "Operating-point D₂ pressure, mTorr. Default 2.0.",
    },
    temperature_k: {
      type: "number" as const,
      description: "Gas temperature, K. Default 300.",
    },
    cx_sigma_m2: {
      type: "number" as const,
      description:
        "Enable charge-exchange channels with this constant cross section, " +
        "m² (order 1e-19 for D⁺ on D₂ in the keV band). Omitted = CX off; " +
        "the neutron-rate claim then reports the beam-on-background floor.",
    },
    step_mm: {
      type: "number" as const,
      description:
        "3D specs only: tracer step length, mm. Default a quarter of the " +
        "thinnest wire radius, capped at 0.25 mm. (nr/nz apply to 2D specs only.)",
    },
  },
};

const documentIdSchema = {
  type: "string" as const,
  description:
    "Session document to extract the device from (instead of `spec`): the " +
    "chamber and grid are measured from the document's own evaluated solids, " +
    "so the simulation runs on the geometry the drawings show. Requires " +
    "`extract`.",
};

const deviceExtractSchema = {
  type: "object" as const,
  required: ["chamber", "electrodes"],
  description:
    "Which parts of `document_id` are the device. Geometry is measured from " +
    "the BRep (literals); potentials and ampere-turns are NAMED DOCUMENT " +
    "PARAMETERS (loon `defparam`), so scrubbing them in the document " +
    "re-prices the run. Fail-closed: anything the axisymmetric device can't " +
    "hold — a ring off the chamber axis (globe/meridian grids), crossing or " +
    "trimmed wires, a stalk fused into the grid part, a non-cylindrical or " +
    "ported chamber, an undeclared parameter — errors naming the part.",
  properties: {
    chamber: {
      type: "string" as const,
      description:
        "Chamber part (name or part id), modelled as a closed can: outer " +
        "cylinder minus a coaxial inner cylinder. Its cavity sets the device " +
        "radius, half-height, axis and origin.",
    },
    wall_potential: {
      type: "string" as const,
      description:
        "Document parameter holding the wall potential, V. Omitted = grounded.",
    },
    electrodes: {
      type: "array" as const,
      minItems: 1,
      items: {
        type: "object" as const,
        required: ["part", "potential"],
        properties: {
          part: {
            type: "string" as const,
            description:
              "Electrode part (name or part id) holding only complete, " +
              "separate toroidal wire rings coaxial with the chamber.",
          },
          potential: {
            type: "string" as const,
            description: "Document parameter holding the potential, V.",
          },
          ampere_turns: {
            type: "string" as const,
            description:
              "Document parameter holding the ring current, ampere-turns " +
              "(+ = CCW viewed from +axis). Every ring in the part shares it; " +
              "put opposed-current rings in separate parts.",
          },
        },
      },
    },
  },
};

export const simulateChargedParticlesSchema = {
  type: "object" as const,
  properties: {
    spec: {
      description:
        "An axisymmetric ring device (`rings`) or a 3D wire device " +
        "(`chamber` + `electrodes`). Pass this OR `document_id` + `extract` " +
        "(extraction is axisymmetric only).",
      anyOf: [deviceSpecSchema, deviceSpec3dSchema],
    },
    document_id: documentIdSchema,
    extract: deviceExtractSchema,
    parameters: {
      ...parametersSchema,
      description:
        "Bindings for named spec parameters, `{name: value}`. With " +
        "`document_id` these override the document's parameter values.",
    },
    options: simOptionsSchema,
  },
};

interface ExtractedDevice {
  spec: unknown;
  parameters: Record<string, number>;
  cavity: unknown;
  ring_parts: string[];
}

/** Resolve the spec + parameters for a particle tool call: the raw `spec`,
 *  or one extracted from the session document's solids. */
function deviceInput(
  a: Json,
  ctx: Parameters<ToolDef["handler"]>[1],
  tool: string,
): { spec: unknown; parameters: Record<string, number>; extracted?: Json } {
  const params = (a.parameters ?? {}) as Record<string, number>;
  if (a.document_id === undefined) {
    if (a.spec === undefined) {
      throw new Error(`${tool}: pass \`spec\`, or \`document_id\` + \`extract\``);
    }
    return { spec: a.spec, parameters: params };
  }
  if (a.spec !== undefined) {
    throw new Error(`${tool}: pass \`spec\` OR \`document_id\`, not both`);
  }
  if (a.extract === undefined) {
    throw new Error(`${tool}: \`document_id\` requires \`extract\` (chamber + electrode parts)`);
  }
  const documentId = String(a.document_id);
  const doc = getSession(documentId);
  const ex = ctx.engine.extractDeviceSpec(
    JSON.stringify(doc),
    JSON.stringify(a.extract),
  ) as ExtractedDevice;
  const parameters = { ...ex.parameters, ...params };
  return {
    spec: ex.spec,
    parameters,
    extracted: {
      document_id: documentId,
      spec: ex.spec,
      parameters,
      cavity: ex.cavity,
      ring_parts: ex.ring_parts,
    },
  };
}

export const optimizeElectrodesSchema = {
  type: "object" as const,
  required: ["variables"],
  properties: {
    spec: {
      ...deviceSpecSchema,
      description:
        deviceSpecSchema.description +
        " Pass this OR `document_id` + `extract`.",
    },
    document_id: documentIdSchema,
    extract: deviceExtractSchema,
    parameters: {
      ...parametersSchema,
      description:
        "Bindings for named spec parameters, `{name: value}`. With " +
        "`document_id` these override the document's parameter values; " +
        "only parameters the extracted spec names (potentials, " +
        "ampere-turns) can be optimized — geometry is measured literals.",
    },
    variables: {
      type: "array" as const,
      minItems: 1,
      description:
        "Named spec parameters to optimize, with box bounds. The objective " +
        "is predicted D-D yield per ion (∫σv dt).",
      items: {
        type: "object" as const,
        required: ["name", "lo", "hi"],
        properties: {
          name: { type: "string" as const },
          lo: { type: "number" as const },
          hi: { type: "number" as const },
          start: {
            type: "number" as const,
            description: "Optional explicit start for the first seed.",
          },
        },
      },
    },
    options: {
      type: "object" as const,
      properties: {
        nr: { type: "number" as const, description: "Default 81." },
        nz: { type: "number" as const, description: "Default 161." },
        particles: { type: "number" as const, description: "Default 48." },
        max_passes: { type: "number" as const, description: "Default 20." },
        max_iters: {
          type: "number" as const,
          description: "Ascent iterations per start. Default 8.",
        },
        multi_start: {
          type: "boolean" as const,
          description:
            "Three seeds across the box instead of one (the landscape is " +
            "multimodal). Default true.",
        },
        gradient: {
          type: "string" as const,
          enum: ["fd", "hybrid"],
          description:
            "Gradient routing. \"fd\" (default): central differences for " +
            "every variable. \"hybrid\": potentials + ampere-turns take the " +
            "discrete adjoint (one run per iterate), geometry takes FD; an " +
            "iteration whose adjoint direction fails to climb is retried on " +
            "pure FD. The adjoint differentiates a fixed-horizon surrogate " +
            "with no wire interception, so on shield current it can disagree " +
            "with the scored objective — each iteration reports its source.",
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
    name: "simulate_charged_particles",
    pack: null,
    description:
      "Charged-particle optics for axisymmetric electrode devices (fusors, magnetically " +
      "shielded-grid IEC, ring traps): solve the electrostatic field + ring-coil magnetics, " +
      "trace a deuteron ensemble (Boris integrator, Bosch-Hale D-D yield weighting), and " +
      "report interception fraction (the cathode-ammeter observable), recirculation, " +
      "predicted neutron rate, fusion power, Q, and distance-to-Lawson — as data plus " +
      "`vcad.particle-claims/1` and unified-receipt claims. Predictions carry basis " +
      "\"predicted\" and roll up Provisional until bench-measured. Vacuum single-particle " +
      "optics: no space charge, no CX chain (optional single-generation CX via " +
      "`options.cx_sigma_m2`); neutron rates are floors. Deterministic. Cost scales with " +
      "grid × particles × passes — defaults run in seconds. A 3D spec (`chamber` + " +
      "`electrodes`) models real globe grids (great circles, latitude + meridian wires) in " +
      "a grounded sphere via the thin-wire BEM and a full-sphere launch; it returns stats, " +
      "neutron-rate floor/ceiling and the solver's surface-residual report, without " +
      "receipt claims (3D claims are not wired yet).",
    inputSchema: simulateChargedParticlesSchema,
    handler: (args, ctx) => {
      const a = args as Json;
      const spec = a.spec as Json | undefined;
      if (a.document_id === undefined && spec && typeof spec === "object" && "electrodes" in spec) {
        return textResult(
          ctx.engine.particleSimulate3d(
            JSON.stringify(spec),
            JSON.stringify(a.parameters ?? {}),
            JSON.stringify(a.options ?? {}),
          ),
        );
      }
      const input = deviceInput(a, ctx, "simulate_charged_particles");
      const out = ctx.engine.particleSimulate(
        JSON.stringify(input.spec),
        JSON.stringify(input.parameters),
        JSON.stringify(a.options ?? {}),
      ) as Json;
      return textResult(input.extracted ? { ...out, extracted: input.extracted } : out);
    },
    behavior: behavior({}),
  },
  {
    name: "optimize_electrodes",
    pack: null,
    description:
      "Design electrodes by gradient ascent: optimize named device-spec parameters (e.g. " +
      "shield ampere-turns, ring spacing) against predicted D-D yield per ion, with " +
      "multi-start search (the yield landscape is multimodal — single starts stall on " +
      "the recirculation hill at ~3x lower yield). Gradients are FD by default; " +
      "`options.gradient: \"hybrid\"` routes potentials + ampere-turns through the " +
      "discrete adjoint with FD for geometry and an FD fallback. Returns the best " +
      "parameter set, its yield, per-start results, the ascent history, and " +
      "per-iteration records (objective + gradient source per variable). Candidate configurations that " +
      "fail to resolve or converge score 0 rather than aborting. Follow with " +
      "simulate_charged_particles at the winning parameters for full claims. Cost ≈ " +
      "evals × one simulation; defaults run in tens of seconds.",
    inputSchema: optimizeElectrodesSchema,
    handler: (args, ctx) => {
      const a = args as Json;
      const input = deviceInput(a, ctx, "optimize_electrodes");
      const out = ctx.engine.particleOptimize(
        JSON.stringify(input.spec),
        JSON.stringify(input.parameters),
        JSON.stringify({
          variables: a.variables,
          ...(a.options as Json | undefined),
        }),
      );
      return textResult(out);
    },
    behavior: behavior({}),
  },
];
