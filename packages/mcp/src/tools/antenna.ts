/**
 * Antenna solver tools — the `vcad-kernel-antenna` crate over MCP.
 *
 * `analyze_antenna` runs the thin-wire method-of-moments solver over a
 * frequency band: Z_in/S11 sweep, in-band resonance, far-field peak
 * gain, radiation-efficiency cross-check, and the
 * `vcad.antenna-claims/1` set plus unified-receipt claims. Predictions
 * carry `basis: "predicted"` and roll up Provisional — never verified —
 * until a VNA measurement is bound.
 *
 * With `document_id` + `extract` instead of `spec`, the antenna is read
 * from the session document's PcbBoard (`vcad_eval::antenna_extract`):
 * antenna-net trace centerlines → wires of radius width/4, delta-gap feed
 * at a pad, infinite ground at the pour edge — with that approximation,
 * the pour's real size in wavelengths, and the board's ε_r/thickness
 * returned under `extracted` (dielectric loading itself is not modelled).
 */

import { behavior, type ToolDef } from "./tool-def.js";
import { getSession } from "./session-core.js";

const paramValue = {
  description:
    "A number, or the name of a named parameter bound via `parameters`.",
  anyOf: [{ type: "number" as const }, { type: "string" as const }],
};

const pointSpec = {
  type: "array" as const,
  minItems: 3,
  maxItems: 3,
  items: paramValue,
  description: "[x, y, z] in mm (Z-up; ground plane, when on, is z=0).",
};

const antennaSpecSchema = {
  type: "object" as const,
  required: ["elements", "feed_mm"],
  description:
    "Thin-wire antenna: straight wires, polyline paths, and closed loops, " +
    "with a delta-gap feed at the basis nearest `feed_mm`. Units mm. Any " +
    "numeric field may instead be a string naming a parameter supplied in " +
    "`parameters` (fail-closed: unbound names error).",
  properties: {
    ground_plane: {
      type: "boolean" as const,
      description:
        "Infinite PEC ground at z=0 (monopoles). Default false (free space).",
    },
    elements: {
      type: "array" as const,
      minItems: 1,
      items: {
        type: "object" as const,
        required: ["type"],
        description:
          'Element: `{"type":"wire","start_mm","end_mm","radius_mm",' +
          '"segments"}`, `{"type":"path","points_mm","radius_mm",' +
          '"segments_per_leg"}` (len = points-1), or `{"type":"loop",...}` ' +
          "(segments_per_leg len = points, closing leg included).",
        properties: {
          type: { type: "string" as const, enum: ["wire", "path", "loop"] },
          start_mm: pointSpec,
          end_mm: pointSpec,
          points_mm: { type: "array" as const, items: pointSpec },
          radius_mm: paramValue,
          segments: { type: "number" as const },
          segments_per_leg: {
            type: "array" as const,
            items: { type: "number" as const },
          },
        },
      },
    },
    feed_mm: pointSpec,
  },
};

type Json = Record<string, unknown>;

const antennaExtractSchema = {
  type: "object" as const,
  required: ["feed"],
  description:
    "Which copper of `document_id`'s PcbBoard is the antenna. Every trace on " +
    "the antenna net becomes a round wire of radius width/4; the feed is a " +
    "delta gap between the feed pad and the ground pour; the pour becomes an " +
    "INFINITE PEC plane at its edge nearest the feed (image theory — recorded " +
    "with the pour's real size in wavelengths). Fail-closed: arcs, vias, " +
    "zones or extra pads on the antenna net, copper on two layers, floating " +
    "islands, copper over/behind the pour edge (feedlines), a pour wrapping " +
    "the antenna, or a feed that isn't a free trace end on the pad all error.",
  properties: {
    feed: {
      type: "object" as const,
      required: ["footprint", "pad"],
      description: "Feed pad, e.g. the U.FL signal pad.",
      properties: {
        footprint: { type: "string" as const, description: "Reference, e.g. J1." },
        pad: { type: "string" as const, description: "Pad number, e.g. 1." },
      },
    },
    net: {
      type: "string" as const,
      description: "Antenna net. Default: the feed pad's net.",
    },
    ground_net: {
      type: "string" as const,
      description: "Net of the ground pour. Default GND.",
    },
    board_node: {
      type: "number" as const,
      description: "PcbBoard node id. Default: the document's only board.",
    },
    segments_per_wavelength: {
      type: "number" as const,
      description: "Segmentation at band top (>= 8). Default 20.",
    },
    max_feed_gap_mm: {
      type: "number" as const,
      description: "Largest feed-pad-to-pour-edge gap accepted, mm. Default 2.",
    },
  },
};

interface ExtractedAntenna {
  spec: unknown;
  parameters: Record<string, number>;
  [k: string]: unknown;
}

/** The spec + parameters for an `analyze_antenna` call: the raw `spec`, or
 *  one extracted from the session document's PcbBoard. */
function antennaInput(
  a: Json,
  ctx: Parameters<ToolDef["handler"]>[1],
): { spec: unknown; parameters: Record<string, number>; extracted?: Json } {
  const params = (a.parameters ?? {}) as Record<string, number>;
  if (a.document_id === undefined) {
    if (a.spec === undefined) {
      throw new Error("analyze_antenna: pass `spec`, or `document_id` + `extract`");
    }
    return { spec: a.spec, parameters: params };
  }
  if (a.spec !== undefined) {
    throw new Error("analyze_antenna: pass `spec` OR `document_id`, not both");
  }
  if (a.extract === undefined) {
    throw new Error(
      "analyze_antenna: `document_id` requires `extract` (at least the feed pad)",
    );
  }
  const band = a.band as { f_hi_hz?: number } | undefined;
  const documentId = String(a.document_id);
  const doc = getSession(documentId);
  const ex = ctx.engine.extractAntennaSpec(
    JSON.stringify(doc),
    JSON.stringify({ ...(a.extract as Json), f_max_hz: band?.f_hi_hz }),
  ) as ExtractedAntenna;
  const { spec, parameters, ...meta } = ex;
  return {
    spec,
    parameters: { ...parameters, ...params },
    extracted: { document_id: documentId, ...meta, spec },
  };
}

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
    name: "analyze_antenna",
    pack: null,
    description:
      "Thin-wire method-of-moments antenna analysis over a frequency band: per-frequency " +
      "Z_in and S11 sweep rows, minimum-S11 point, in-band resonance (Im Z = 0), -10 dB " +
      "bandwidth, far-field peak gain (37x72 pattern scan), and a radiation-efficiency " +
      "energy cross-check — as data plus `vcad.antenna-claims/1` and unified-receipt " +
      "claims. Predictions carry basis \"predicted\" and roll up Provisional until a VNA " +
      "sweep is bound. Pass `spec` (hand-built wires), or `document_id` + `extract` to read " +
      "the antenna from the document's PCB board (traces -> wires, pad feed, infinite " +
      "ground at the pour edge; approximation + board eps_r/thickness returned under " +
      "`extracted`). Free-space PEC wires (optional infinite ground plane): no dielectric " +
      "substrate (FR-4 PCB antennas resonate ~20-30% BELOW the prediction), no " +
      "conductor loss (gain = directivity), thin-wire validity gates enforced " +
      "fail-closed (segment >= 4a, segment <= lambda/8, ka <= 0.1). Deterministic. Cost " +
      "~O(segments^3) per frequency x band points (segment cap 600); a 30-segment " +
      "dipole over 46 points runs in seconds.",
    inputSchema: {
      type: "object" as const,
      required: ["band"],
      properties: {
        spec: {
          ...antennaSpecSchema,
          description:
            antennaSpecSchema.description + " Pass this OR `document_id` + `extract`.",
        },
        document_id: {
          type: "string" as const,
          description:
            "Session document whose PcbBoard holds the antenna (instead of `spec`). " +
            "Requires `extract`.",
        },
        extract: antennaExtractSchema,
        parameters: {
          type: "object" as const,
          additionalProperties: { type: "number" as const },
          description:
            "Bindings for named spec parameters, `{name: value}` (an extracted board " +
            "spec is all literals).",
        },
        band: {
          type: "object" as const,
          required: ["f_lo_hz", "f_hi_hz", "points"],
          description:
            "Frequency band to sweep and make claims over (with `document_id`, " +
            "`f_hi_hz` also sets the extracted segmentation).",
          properties: {
            f_lo_hz: { type: "number" as const },
            f_hi_hz: { type: "number" as const },
            points: {
              type: "number" as const,
              description: "Sweep points across the band (2..2000).",
            },
          },
        },
        options: {
          type: "object" as const,
          properties: {
            z0: {
              type: "number" as const,
              description: "S11 reference impedance, ohm. Default 50.",
            },
            quad_outer: {
              type: "number" as const,
              description: "Outer Gauss-Legendre order. Default 6.",
            },
            quad_inner: {
              type: "number" as const,
              description: "Inner Gauss-Legendre order. Default 6.",
            },
            sweep: {
              type: "boolean" as const,
              description:
                "Return per-frequency sweep rows (Z_in, S11). Default true.",
            },
          },
        },
      },
    },
    handler: (args, ctx) => {
      const a = args as Json;
      const input = antennaInput(a, ctx);
      const out = ctx.engine.antennaAnalyze(
        JSON.stringify(input.spec),
        JSON.stringify(input.parameters),
        JSON.stringify({ band: a.band, ...(a.options as Json | undefined) }),
      ) as Json;
      return textResult(input.extracted ? { ...out, extracted: input.extracted } : out);
    },
    behavior: behavior({}),
  },
];
