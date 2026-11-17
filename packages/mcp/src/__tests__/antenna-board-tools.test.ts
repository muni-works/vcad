import { describe, it, expect, beforeAll } from "vitest";
import { Engine } from "@vcad/engine";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { createServer } from "../server.js";
import { documents } from "../tools/session.js";

/**
 * PCB → antenna over MCP: a printed monopole drawn with the ordinary ECAD
 * tools (create_schematic → place_components → set_placement → add_zone →
 * add_trace), then `analyze_antenna { document_id, extract }` reads it off
 * the PcbBoard (`vcad_eval::antenna_extract`). The extracted run must agree
 * with the same geometry hand-built as a `spec` to solver precision, and
 * carry the ground approximation + substrate metadata.
 *
 * Skips when the loaded kernel WASM predates the extraction binding (the
 * checked-in artifacts refresh only on main; CI builds from source).
 */

const probeEngine = await Engine.init();
const wasmHasExtract =
  typeof (
    probeEngine as unknown as {
      kernel?: { extractAntennaSpec?: unknown };
    }
  ).kernel?.extractAntennaSpec === "function";

type ToolOut = { isError?: boolean; content: Array<{ type: string; text: string }> };

const BAND = { f_lo_hz: 2.2e9, f_hi_hz: 3.0e9, points: 5 };

describe.skipIf(!wasmHasExtract)("analyze_antenna from a PCB board", () => {
  let call: (name: string, args: Record<string, unknown>) => Promise<ToolOut>;
  let id: string;
  const json = (r: ToolOut) => JSON.parse(r.content[0].text) as Record<string, any>;

  beforeAll(async () => {
    documents.clear();
    const engine = await Engine.init();
    const server = await createServer(engine, { user: null });
    const [clientT, serverT] = InMemoryTransport.createLinkedPair();
    const client = new Client({ name: "antenna-board", version: "0.0.0" }, { capabilities: {} });
    await Promise.all([client.connect(clientT), server.connect(serverT)]);
    call = async (name, args) => (await client.callTool({ name, arguments: args })) as ToolOut;

    // A two-pad feed part (stand-in for a U.FL): pad 1 = ANT, pad 2 = GND.
    const sch = await call("create_schematic", {
      components: [
        {
          ref: "J1",
          value: "feed",
          footprint: "Resistor_SMD:R_0805",
          x: 0,
          y: 0,
          pins: [
            { number: "1", name: "SIG", type: "Passive", x: -5, y: 0 },
            { number: "2", name: "GND", type: "Passive", x: 5, y: 0 },
          ],
        },
      ],
      nets: { ANT: ["J1.1"], GND: ["J1.2"] },
    });
    expect(sch.isError, sch.content[0]?.text).toBeFalsy();
    id = json(sch).document_id;
    const placed = await call("place_components", {
      document_id: id,
      board_width: 50,
      board_height: 50,
    });
    expect(placed.isError, placed.content[0]?.text).toBeFalsy();
    const moved = await call("set_placement", {
      document_id: id,
      placements: [{ ref: "J1", x: 25, y: 30, rotation: 0 }],
    });
    expect(moved.isError, moved.content[0]?.text).toBeFalsy();

    const pads = json(await call("get_pad_positions", { document_id: id, ref: "J1" }))
      .pads as Array<{ pin: string; x: number; y: number }>;
    const p1 = pads.find((p) => p.pin === "1")!;
    const p2 = pads.find((p) => p.pin === "2")!;
    // d: away from the GND pad (into the keepout); t: along the pour edge.
    const len = Math.hypot(p1.x - p2.x, p1.y - p2.y);
    const d = { x: (p1.x - p2.x) / len, y: (p1.y - p2.y) / len };
    const t = { x: -d.y, y: d.x };
    const at = (a: number, b: number) => ({
      x: p1.x + a * d.x + b * t.x,
      y: p1.y + a * d.y + b * t.y,
    });
    // Pour behind the edge line through pad 1 (pad 2 sits on the pour).
    const zone = await call("add_zone", {
      document_id: id,
      net: "GND",
      layer: "FCu",
      outline: [at(0, -12), at(0, 12), at(-15, 12), at(-15, -12)],
    });
    expect(zone.isError, zone.content[0]?.text).toBeFalsy();
    // L monopole: 12 mm out from the edge, then 10 mm along it.
    const tr = await call("add_trace", {
      document_id: id,
      net: "ANT",
      layer: "FCu",
      width: 1.0,
      points: [at(0, 0), at(12, 0), at(12, 10)],
    });
    expect(tr.isError, tr.content[0]?.text).toBeFalsy();
  });

  it("matches the hand-built spec and carries the approximation", async () => {
    const res = await call("analyze_antenna", {
      document_id: id,
      extract: { feed: { footprint: "J1", pad: "1" } },
      band: BAND,
    });
    expect(res.isError, res.content[0].text).toBeFalsy();
    const out = json(res);
    expect(out.claim_set.schema).toBe("vcad.antenna-claims/1");
    expect(out.receipt_claims.length).toBeGreaterThan(0);
    const ex = out.extracted;
    expect(ex.document_id).toBe(id);
    expect(ex.net).toBe("ANT");
    expect(ex.feed.treatment).toBe("on_edge");
    expect(ex.ground.model).toBe("infinite_image");
    expect(ex.ground.pour_along_edge_mm).toBeCloseTo(24, 6);
    expect(typeof ex.substrate.board_thickness_mm).toBe("number");
    expect(ex.notes.join(" ")).toMatch(/BELOW/);

    // Same geometry by hand (mirror-symmetric over the plane, so the
    // along-edge sign is immaterial), same segment counts.
    const segs = (ex.spec.elements as Array<{ segments: number }>).map((e) => e.segments);
    const hand = await call("analyze_antenna", {
      spec: {
        ground_plane: true,
        elements: [
          { type: "wire", start_mm: [0, 0, 0], end_mm: [0, 0, 12], radius_mm: 0.25, segments: segs[0] },
          { type: "wire", start_mm: [0, 0, 12], end_mm: [10, 0, 12], radius_mm: 0.25, segments: segs[1] },
        ],
        feed_mm: [0, 0, 0],
      },
      band: BAND,
    });
    expect(hand.isError, hand.content[0].text).toBeFalsy();
    const h = json(hand);
    expect(out.sweep.length).toBe(h.sweep.length);
    for (let i = 0; i < h.sweep.length; i++) {
      const a = out.sweep[i];
      const b = h.sweep[i];
      const rel = Math.hypot(a.z_re_ohm - b.z_re_ohm, a.z_im_ohm - b.z_im_ohm) /
        Math.hypot(b.z_re_ohm, b.z_im_ohm);
      expect(rel, `Z_in at ${b.freq_hz}`).toBeLessThan(1e-9);
    }
  });

  it("fails closed on a feed pad off the antenna net", async () => {
    const res = await call("analyze_antenna", {
      document_id: id,
      extract: { feed: { footprint: "J1", pad: "2" } },
      band: BAND,
    });
    expect(res.isError).toBe(true);
    expect(res.content[0].text).toMatch(/ground net/);
  });

  it("rejects spec and document_id together", async () => {
    const res = await call("analyze_antenna", {
      document_id: id,
      extract: { feed: { footprint: "J1", pad: "1" } },
      spec: { elements: [], feed_mm: [0, 0, 0] },
      band: BAND,
    });
    expect(res.isError).toBe(true);
  });
});
