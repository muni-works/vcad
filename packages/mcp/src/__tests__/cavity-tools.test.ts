import { describe, it, expect, beforeAll } from "vitest";
import { Engine } from "@vcad/engine";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createServer } from "../server.js";
import { documents } from "../tools/session.js";

/**
 * End-to-end coverage for `simulate_cavity`: the real server, the real WASM
 * kernel, the "every physics" puck (a cylindrical bass-reflex speaker with
 * an internal port) authored in loon, extracted from the session document
 * and solved.
 *
 * Skips (rather than fails) when the loaded kernel WASM predates the cavity
 * binding. Locally: `npm run build -w vcad-kernel-wasm`.
 */

const probeEngine = await Engine.init();
const wasmHasCavity =
  typeof (
    probeEngine as unknown as { kernel?: { simulateCavity?: unknown } }
  ).kernel?.simulateCavity === "function";

const PUCK = readFileSync(
  resolve(__dirname, "../../../../crates/vcad-eval/tests/fixtures/puck.loon"),
  "utf8",
);
// v2: the speaker motor sits inside the box and is carved out of the air.
const PUCK_V2 = readFileSync(
  resolve(__dirname, "../../../../crates/vcad-eval/tests/fixtures/puck_v2.loon"),
  "utf8",
);

type ToolResult = { isError?: boolean; content: Array<{ type: string; text: string }> };

describe.skipIf(!wasmHasCavity)("cavity acoustics tool", () => {
  let client: Client;
  let id: string;

  beforeAll(async () => {
    documents.clear();
    const engine = await Engine.init();
    const server = await createServer(engine, { user: null });
    const [clientT, serverT] = InMemoryTransport.createLinkedPair();
    client = new Client({ name: "cavity-tools", version: "0.0.0" }, { capabilities: {} });
    await Promise.all([client.connect(clientT), server.connect(serverT)]);
    const opened = (await client.callTool({ name: "open_document", arguments: {} })) as ToolResult;
    id = JSON.parse(opened.content[0].text).document_id;
    const made = (await client.callTool({
      name: "create_cad_loon",
      arguments: { document_id: id, mode: "add", source: PUCK },
    })) as ToolResult;
    expect(made.isError, made.content[0]?.text).toBeFalsy();
  }, 180_000);

  it("extracts the puck's internal port and tunes it", async () => {
    const res = (await client.callTool({
      name: "simulate_cavity",
      arguments: {
        document_id: id,
        enclosure: "enclosure",
        air: "air",
        driver_radius: "driver_r",
        options: { sweep_points: 28, curve_points: 12 },
      },
    })) as ToolResult;
    expect(res.isError, res.content[0]?.text).toBeFalsy();
    const out = JSON.parse(res.content[0].text);
    const { extracted, report } = out;
    // V_box = the air solid: π(52²·114 − 14²·57) mm³ ≈ 0.933 L.
    const vBox = Math.PI * (52 ** 2 * 114 - 14 ** 2 * 57);
    expect(Math.abs(extracted.air_volume_mm3 - vBox) / vBox).toBeLessThan(1e-6);
    expect(extracted.model.mount).toBe("internal");
    expect(extracted.model.port_radius_mm).toBeCloseTo(12, 9);
    expect(extracted.model.port_length_mm).toBeCloseTo(60, 9);
    expect(extracted.model.spec.obstacles.length).toBe(1);
    // Lumped nominal matches the PoC hand mapping (136.6 Hz).
    expect(report.lumped.f_nominal_hz).toBeCloseTo(136.6, 1);
    expect(report.field_tuning_hz).toBeGreaterThan(report.lumped.f_min_hz);
    expect(report.field_tuning_hz).toBeLessThan(1.25 * report.lumped.f_max_hz);
    expect(report.port_velocity.length).toBe(12);
    expect(report.claims.schema).toBe("vcad.acoustics-claims/1");
    const names = report.claims.claims.map((c: { name: string }) => c.name);
    expect(names).toContain("tuning_hz");
    expect(names).toContain("tuning_hz_lumped");
  }, 600_000);

  it("treats listed motor parts as rigid obstacles (v2 puck)", async () => {
    const opened = (await client.callTool({ name: "open_document", arguments: {} })) as ToolResult;
    const v2 = JSON.parse(opened.content[0].text).document_id;
    const made = (await client.callTool({
      name: "create_cad_loon",
      arguments: { document_id: v2, mode: "add", source: PUCK_V2 },
    })) as ToolResult;
    expect(made.isError, made.content[0]?.text).toBeFalsy();
    const base = { document_id: v2, enclosure: "enclosure", air: "air", driver_radius: "driver_r" };
    // Without the motor listed, the carved-out void is refused.
    const bare = (await client.callTool({ name: "simulate_cavity", arguments: base })) as ToolResult;
    expect(bare.isError).toBe(true);
    expect(bare.content[0].text).toContain("obstacles");
    const res = (await client.callTool({
      name: "simulate_cavity",
      arguments: {
        ...base,
        obstacles: ["yoke", "magnet", "top_plate", "voice_coil"],
        options: { sweep_points: 20, curve_points: 4 },
      },
    })) as ToolResult;
    expect(res.isError, res.content[0]?.text).toBeFalsy();
    const { extracted, report } = JSON.parse(res.content[0].text);
    // V_box = π(52²·114 − 14²·57 − 30²·18) mm³: the motor envelope is out.
    const vBox = Math.PI * (52 ** 2 * 114 - 14 ** 2 * 57 - 30 ** 2 * 18);
    expect(Math.abs(extracted.air_volume_mm3 - vBox) / vBox).toBeLessThan(1e-6);
    expect(extracted.model.spec.obstacles.length).toBe(2);
    expect(report.field_tuning_hz).toBeGreaterThan(report.lumped.f_nominal_hz);
  }, 600_000);

  it("fails closed naming the problem", async () => {
    const res = (await client.callTool({
      name: "simulate_cavity",
      arguments: { document_id: id, enclosure: "enclosure", air: "air", driver_radius: "cone_r" },
    })) as ToolResult;
    expect(res.isError).toBe(true);
    expect(res.content[0].text).toContain("cone_r");
  }, 180_000);
});
