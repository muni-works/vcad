/**
 * simulate_cavity — bass-reflex box tuning straight from the document.
 *
 * The air-side half of the acoustics stack (`vcad-kernel-acoustics`): the
 * axisymmetric Helmholtz field solver and the lumped duct/cavity oracles.
 * Nothing here is hand-mapped: `vcad_eval::cavity_extract` measures the
 * enclosure and air parts' analytic BRep surfaces, reconstructs the exact
 * axisymmetric profile (box segments, port bore, and an internal port's
 * tube wall as a rigid annular obstacle), and the kernel solves it. All
 * numerics run in the kernel through the WASM binding `simulateCavity`;
 * this file only marshals.
 */

import { behavior, type ToolDef } from "./tool-def.js";
import { getSession } from "./session-core.js";

type Json = Record<string, unknown>;

const paramOrNumber = {
  anyOf: [{ type: "number" as const }, { type: "string" as const }],
};

export const toolDefs: ToolDef[] = [
  {
    name: "simulate_cavity",
    pack: null,
    description:
      "Bass-reflex (ported box) tuning from a session document: measures the enclosure " +
      "and enclosed-air parts' BRep, reconstructs the exact axisymmetric field geometry " +
      "(box, port bore, an INTERNAL port's tube wall as a rigid annular obstacle — no " +
      "equivalent-volume mapping), and returns the lumped tuning band (V_box = the air " +
      "part's volume; mount-aware end corrections), the field-solved tuning f_b, the port " +
      "volume-velocity curve, and `vcad.acoustics-claims/1` predicted claims. Fail-closed: " +
      "any face that is not a plane perpendicular to the axis or a coaxial cylinder (box " +
      "enclosure, off-axis port, fillet) errors naming the face; a gap between air and " +
      "wall, a side vent, or a second port errors naming where. Model: linear, lossless " +
      "(Q optimistic), rigid walls and piston, pressure-release port mouth (field f_b " +
      "reads above the lumped nominal). Predictions carry basis \"predicted\" and roll up " +
      "Provisional until a mic + swept-sine measurement binds them. Seconds at ~1 mm grid.",
    inputSchema: {
      type: "object" as const,
      required: ["document_id", "enclosure", "air"],
      properties: {
        document_id: {
          type: "string" as const,
          description: "Session document holding the speaker.",
        },
        enclosure: {
          type: "string" as const,
          description:
            "The enclosure walls part: name or part id (or, when no part has that name, " +
            "the material of exactly one part).",
        },
        air: {
          type: "string" as const,
          description:
            "The enclosed box air part (cavity minus the port tube): name, part id, or " +
            "unique material such as \"air\". Its volume is the lumped V_box.",
        },
        driver_radius: {
          ...paramOrNumber,
          description:
            "Driver piston radius: mm, or the name of a document parameter (stays bound, " +
            "so `parameters` overrides re-price it). Default: the driver cutout radius.",
        },
        temp_c: {
          ...paramOrNumber,
          description: "Air temperature °C, or a parameter name. Default 20.",
        },
        parameters: {
          type: "object" as const,
          additionalProperties: { type: "number" as const },
          description:
            "Overrides for the document's parameter values when resolving named spec " +
            "fields (driver_radius / temp_c). Geometry is measured from the document as " +
            "evaluated — change geometry with set_parameters.",
        },
        options: {
          type: "object" as const,
          description: "Solver knobs (all optional).",
          properties: {
            grid_mm: {
              type: "number" as const,
              description:
                "Node spacing mm (default: half the smallest feature, so every port wall " +
                "holds a node; a coarser grid that drops a wall fails closed).",
            },
            search_hz: {
              type: "array" as const,
              items: { type: "number" as const },
              minItems: 2,
              maxItems: 2,
              description: "Tuning search band [lo, hi] Hz (default 0.5·f_min … 2·f_max).",
            },
            curve_hz: {
              type: "array" as const,
              items: { type: "number" as const },
              minItems: 2,
              maxItems: 2,
              description: "Port velocity curve band [lo, hi] Hz (default: search band).",
            },
            curve_points: {
              type: "number" as const,
              description: "Port velocity curve samples (default 60).",
            },
            sweep_points: {
              type: "number" as const,
              description: "Coarse tuning-sweep samples before refinement (default 48).",
            },
          },
        },
      },
    },
    handler: (args, ctx) => {
      const a = args as Json;
      const documentId = String(a.document_id);
      const selection: Json = {
        enclosure: String(a.enclosure),
        air: String(a.air),
      };
      if (a.driver_radius !== undefined) selection.driver_radius = a.driver_radius;
      if (a.temp_c !== undefined) selection.temp_c = a.temp_c;
      const out = ctx.engine.simulateCavity(
        JSON.stringify(getSession(documentId)),
        JSON.stringify(selection),
        JSON.stringify(a.options ?? {}),
        JSON.stringify(a.parameters ?? {}),
      ) as Json;
      return {
        content: [
          {
            type: "text" as const,
            text: JSON.stringify({ document_id: documentId, ...out }, null, 2),
          },
        ],
      };
    },
    behavior: behavior({}),
  },
];
