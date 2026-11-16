import { describe, it, expect, beforeAll } from "vitest";
import { Engine } from "@vcad/engine";
import { describePartTool, searchPartsTool } from "../tools/parts.js";

/**
 * Vacuum hardware through the MCP parts tools. The checked-in kernel WASM is
 * refreshed on main after kernel merges, so a branch that adds the vacuum
 * library (and the `partInfo` export) runs these only against WASM built
 * from source — which is what CI's TypeScript job uses.
 */

let engine: Engine;
let hasVacuum = false;

beforeAll(async () => {
  engine = await Engine.init();
  const k = (engine as unknown as { kernel?: Record<string, unknown> }).kernel;
  hasVacuum =
    typeof k?.partInfo === "function" &&
    String((k?.getPartsManifest as () => string)?.() ?? "").includes("vacuum.cf.flange");
});

function json(r: { content: Array<{ text: string }> }) {
  return JSON.parse(r.content[0]!.text);
}

describe("vacuum parts", () => {
  it("search_parts finds CF hardware by synonym", (ctx) => {
    if (!hasVacuum) ctx.skip();
    const r = json(searchPartsTool({ query: "conflat" }, engine));
    expect(r.results.map((p: { id: string }) => p.id)).toContain("std:vacuum.cf.flange");
  });

  it("describe_part returns ports and sourced metadata", (ctx) => {
    if (!hasVacuum) ctx.skip();
    const r = json(
      describePartTool(
        { path: "std:vacuum.chamber.spherical-cube", params: { size: "DN100" } },
        engine,
      ),
    );
    expect(r.ports).toHaveLength(6);
    expect(r.ports[0]).toMatchObject({ family: "CF", size: "DN100" });
    expect(r.metadata.center_to_face_provenance).toMatch(/derived/);
  });

  it("HV feedthrough echoes its rating and suggests a clearance", (ctx) => {
    if (!hasVacuum) ctx.skip();
    const r = json(
      describePartTool({ path: "std:vacuum.feedthrough.hv", params: { rated_kv: 40 } }, engine),
    );
    expect(r.metadata.rated_kv).toBe(40);
    expect(r.metadata.suggested_clearance[0].min_mm).toBe(40);
  });

  it("describe_part rejects unknown parts", () => {
    expect(() => describePartTool({ path: "std:vacuum.nope" }, engine)).toThrow(/unknown part/);
  });
});
