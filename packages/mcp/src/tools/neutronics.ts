/**
 * Neutronics tools — the `vcad-kernel-neutronics` crate over MCP.
 *
 * `simulate_neutron_shield` runs analog Monte Carlo neutron transport
 * through a spherical shield stack and returns dose rates at detector
 * shells WITH batch-statistics error bars, plus the
 * `vcad.neutronics-claims/1` set and unified-receipt claims. Fail-closed
 * throughout: truncated histories or unscored tallies refuse to price
 * claims (the fix is more histories). Predictions carry
 * `basis: "predicted"` and roll up Provisional — never verified — until
 * a survey meter measurement is bound.
 */

import { behavior, type ToolDef } from "./tool-def.js";
import { getSession } from "./session-core.js";

const paramValue = {
  description:
    "A number, or the name of a named parameter bound via `parameters`.",
  anyOf: [{ type: "number" as const }, { type: "string" as const }],
};

const shieldSpecSchema = {
  type: "object" as const,
  required: ["layers", "source", "detectors"],
  description:
    "Spherical shield: concentric layers from r=0 outward around a point " +
    "source, with detector shells that must each sit strictly inside one " +
    "layer (put them in air gaps). Units mm. Any numeric field may " +
    "instead be a string naming a parameter supplied in `parameters` " +
    "(fail-closed: unbound names error).",
  properties: {
    layers: {
      type: "array" as const,
      minItems: 1,
      items: {
        type: "object" as const,
        required: ["material", "thickness_mm"],
        properties: {
          material: {
            type: "string" as const,
            description:
              "Built-in library material: hdpe, paraffin, borated-hdpe-5, " +
              "water, lead, concrete, air, void.",
          },
          thickness_mm: paramValue,
        },
      },
    },
    source: {
      type: "object" as const,
      required: ["rate_n_per_s", "energy_ev"],
      description:
        "Isotropic point source at r=0. Energy must lie in the 5-group " +
        "structure [1e-4, 3e6] eV — D-D 2.45e6 eV is in range; D-T 14.1 " +
        "MeV is rejected.",
      properties: {
        rate_n_per_s: paramValue,
        energy_ev: paramValue,
      },
    },
    detectors: {
      type: "array" as const,
      minItems: 1,
      items: {
        type: "object" as const,
        required: ["label", "radius_mm"],
        properties: {
          label: { type: "string" as const },
          radius_mm: paramValue,
          half_width_mm: {
            ...paramValue,
            description: "Shell half-thickness. Default 20 mm.",
          },
        },
      },
    },
    run: {
      type: "object" as const,
      description: "Monte Carlo controls (deterministic for a given seed).",
      properties: {
        histories_per_batch: {
          type: "number" as const,
          description: "Default 20000.",
        },
        batches: {
          type: "number" as const,
          description: "Error-bar batches (>= 2). Default 20.",
        },
        seed: { type: "number" as const, description: "Default 20260717." },
      },
    },
  },
};

const shieldExtractSchema = {
  type: "object" as const,
  required: ["shells", "source", "detectors"],
  description:
    "Which parts of `document_id` are the shield. Each shell part must be a " +
    "concentric spherical shell (sphere minus a smaller concentric sphere, " +
    "or a solid ball); thicknesses are measured from the BRep. Fail-closed: " +
    "a cylindrical/box shield, an off-centre or overlapping shell, or a " +
    "material outside the library errors naming the part — the transport " +
    "is 1-D spherical, so nothing is folded into a sphere silently.",
  properties: {
    shells: {
      type: "array" as const,
      minItems: 1,
      items: {
        type: "object" as const,
        required: ["part"],
        properties: {
          part: {
            type: "string" as const,
            description: "Shell part (name or part id).",
          },
          material: {
            type: "string" as const,
            description:
              "Library material (hdpe, paraffin, borated-hdpe-5, water, lead, " +
              "concrete, air, void). Omitted = the part's document material.",
          },
        },
      },
    },
    fill_material: {
      type: "string" as const,
      description:
        "Material for the core, the gaps between shells and the world beyond " +
        "the last shell. Default \"air\".",
    },
    source: shieldSpecSchema.properties.source,
    detectors: shieldSpecSchema.properties.detectors,
    world_radius_mm: {
      type: "number" as const,
      description:
        "Outer radius of the modelled world, mm. Default: 10% past the " +
        "outermost detector shell.",
    },
    center: {
      type: "array" as const,
      items: { type: "number" as const },
      minItems: 3,
      maxItems: 3,
      description:
        "Source point [x, y, z] mm (e.g. the chamber centre); every shell " +
        "must be centred on it. Default: the shells' common centre.",
    },
    run: shieldSpecSchema.properties.run,
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
    name: "simulate_neutron_shield",
    pack: null,
    description:
      "Monte Carlo neutron shielding run (analog transport, 5-group multigroup library " +
      "with exact elastic kinematics): spherical layer stack around a point source (D-D " +
      "band), H*(10) dose rate at each detector shell WITH 1-sigma batch error bars " +
      "(rse), attenuation factor vs bare source, thermal flux, and absorbed/leakage " +
      "balance — as data plus `vcad.neutronics-claims/1` and unified-receipt claims. " +
      "Fail-closed: truncated histories or a tally that scored nothing refuses claims " +
      "(raise histories). Predictions carry basis \"predicted\" and roll up Provisional " +
      "until surveyed. Design-estimate physics: 5-group library (not ENDF continuous-" +
      "energy), spherical geometry only, no gamma dose, no activation, source energy " +
      "capped at 3 MeV (no D-T). Deterministic per seed. Cost ~histories (cap 5M); " +
      "error bars shrink as 1/sqrt(N). The 400k default runs in seconds.",
    inputSchema: {
      type: "object" as const,
      properties: {
        spec: {
          ...shieldSpecSchema,
          description:
            shieldSpecSchema.description +
            " Pass this OR `document_id` + `extract`.",
        },
        document_id: {
          type: "string" as const,
          description:
            "Session document to extract the shield from (instead of `spec`): " +
            "the layer stack is measured from the document's own evaluated " +
            "solids. Requires `extract`.",
        },
        extract: shieldExtractSchema,
        parameters: {
          type: "object" as const,
          additionalProperties: { type: "number" as const },
          description:
            "Bindings for named spec parameters, `{name: value}`. With " +
            "`document_id` these override the document's parameter values.",
        },
      },
    },
    handler: (args, ctx) => {
      const a = args as Json;
      const params = (a.parameters ?? {}) as Record<string, number>;
      if (a.document_id === undefined) {
        if (a.spec === undefined) {
          throw new Error(
            "simulate_neutron_shield: pass `spec`, or `document_id` + `extract`",
          );
        }
        return textResult(
          ctx.engine.neutronicsSimulate(JSON.stringify(a.spec), JSON.stringify(params)),
        );
      }
      if (a.spec !== undefined) {
        throw new Error("simulate_neutron_shield: pass `spec` OR `document_id`, not both");
      }
      if (a.extract === undefined) {
        throw new Error(
          "simulate_neutron_shield: `document_id` requires `extract` (shell parts, source, detectors)",
        );
      }
      const documentId = String(a.document_id);
      const ex = ctx.engine.extractShieldSpec(
        JSON.stringify(getSession(documentId)),
        JSON.stringify(a.extract),
      ) as {
        spec: unknown;
        parameters: Record<string, number>;
        center: number[];
        layer_parts: Array<string | null>;
      };
      const parameters = { ...ex.parameters, ...params };
      const out = ctx.engine.neutronicsSimulate(
        JSON.stringify(ex.spec),
        JSON.stringify(parameters),
      ) as Json;
      return textResult({
        ...out,
        extracted: {
          document_id: documentId,
          spec: ex.spec,
          parameters,
          center: ex.center,
          layer_parts: ex.layer_parts,
        },
      });
    },
    behavior: behavior({}),
  },
];
