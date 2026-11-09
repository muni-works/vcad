/**
 * Regressions from an end-to-end agent run: each block pins one defect where
 * a tool lied, crashed, or lost data on an ordinary call sequence.
 */
import { existsSync, mkdtempSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";
import { Engine } from "@vcad/engine";
import { getPcbNodeIds } from "@vcad/core";
import type { Document } from "@vcad/ir";
import { createServer } from "../server.js";
import type { SessionStore } from "../session-store.js";
import { checkClearance } from "../tools/clearance.js";
import {
  boardFromSolid,
  buildReceipt,
  createSchematic,
  createSchematicSchema,
  exportGerber,
  placeComponents,
  runDrc,
  toolDefs as ecadToolDefs,
} from "../tools/ecad.js";
import { checkEnclosureFit } from "../tools/enclosure.js";
import {
  batchCreateEnvsSchema,
  createRobotEnv,
  createRobotEnvSchema,
} from "../tools/gym.js";
import { computeInspection } from "../tools/inspect.js";
import { carryElectronics, toolDefs as loonToolDefs } from "../tools/loon.js";
import { setParameters } from "../tools/parameters.js";
import { getSession, registerSession } from "../tools/session-core.js";
import { loadDocument } from "../tools/session.js";
import { markSourceDiverged } from "../tools/source-provenance.js";
import type { ToolContext } from "../tools/tool-def.js";
import { resolvePartMesh } from "../tools/topopt.js";

let engine: Engine;
let ctx: ToolContext;
const createCadLoon = loonToolDefs.find((t) => t.name === "create_cad_loon")!;

beforeAll(async () => {
  engine = await Engine.init();
  ctx = { engine, user: null } as unknown as ToolContext;
});

afterEach(() => {
  delete process.env.VCAD_MCP_REMOTE;
});

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const out = (r: { content: Array<{ text: string }> }): any => JSON.parse(r.content[0]!.text);

const resistor = (ref: string, x: number, nets: [string, string]) => ({
  ref,
  value: "1k",
  footprint: "Resistor_SMD:R_0805",
  x,
  y: 0,
  pins: [
    { number: "1", name: nets[0], type: "Passive", x: -5, y: 0 },
    { number: "2", name: nets[1], type: "Passive", x: 5, y: 0 },
  ],
});

async function boardSession(): Promise<string> {
  const { document_id: id } = out(
    await createSchematic({ components: [resistor("R1", 0, ["A", "B"])] }),
  );
  out(await placeComponents({ document_id: id, board_width: 30, board_height: 30 }));
  return id as string;
}

/** A 60x40x20 tray open at the top, as one assembly part. */
const ENCLOSURE_ASSEMBLY = `
[let tray [pipe [cube 60 40 20] [difference [pipe [cube 50 30 30] [translate 5 5 4]]]]]
[let thigh [cube 10 10 80]]
[assembly
  #[[part "tray" tray "aluminum"] [part "thigh" thigh "steel"]]
  #[[instance "tray1" "tray" 0 0 0] [instance "thigh1" "thigh" 100 0 0]]
  #[]
  "tray1"]`;

function assemblySession(source = ENCLOSURE_ASSEMBLY): string {
  const doc = engine.evalLoonDocument(source, {}, {});
  if (!doc) throw new Error("loon eval failed");
  return registerSession(doc);
}

describe("create_cad_loon on a session holding a board", () => {
  it("keeps the board and schematic, and the document still answers run_drc", async () => {
    const id = await boardSession();
    await createCadLoon.handler({ document_id: id, source: "[cube 10 10 10]" }, ctx);

    const doc = getSession(id);
    expect(getPcbNodeIds(doc)).toHaveLength(1);
    expect(doc.schematic).toBeDefined();
    expect(Object.values(doc.nodes).some((n) => n.op.type === "Cube")).toBe(true);
    const drc = await runDrc({ document_id: id });
    expect(drc.content[0]!.text).not.toContain("no PCB");
    expect(computeInspection(doc, engine).parts).toBeGreaterThan(0);
  });

  it("holds an assembly and a board in one document", async () => {
    const id = await boardSession();
    await createCadLoon.handler({ document_id: id, source: ENCLOSURE_ASSEMBLY }, ctx);
    const doc = getSession(id);
    expect(getPcbNodeIds(doc)).toHaveLength(1);
    expect(doc.instances).toHaveLength(2);
    expect(resolvePartMesh(doc, engine, "thigh").mesh.positions.length).toBeGreaterThan(0);
  });

  it("fails loudly and writes nothing when the program brings its own schematic", () => {
    const session = { schematic: { components: [] }, nodes: {}, roots: [], materials: {} };
    const loon = { schematic: { components: [] }, nodes: {}, roots: [], materials: {} };
    expect(() =>
      carryElectronics(session as unknown as Document, loon as unknown as Document),
    ).toThrow(/can't be merged/);
  });
});

describe("assembly parts are visible to document tools", () => {
  it("board_from_solid finds a part by part def and by instance", () => {
    const id = assemblySession();
    for (const part_id of ["tray", "tray1"]) {
      const res = boardFromSolid({ document_id: id, part_id }, engine);
      expect(res.isError).toBeFalsy();
    }
  });

  it("check_enclosure_fit reads the enclosure from an assembly", async () => {
    const board = await boardSession();
    const enclosure = assemblySession();
    const res = await checkEnclosureFit(
      { document_id: board, enclosure_document_id: enclosure, enclosure_part_id: "tray" },
      engine,
    );
    expect(res.isError).toBeFalsy();
    expect(out(res).cavity).toBeDefined();
  });

  it("analyze_structure's resolver finds a part world-posed, and misses loudly", () => {
    const doc = getSession(assemblySession());
    const { mesh } = resolvePartMesh(doc, engine, "thigh");
    let minX = Infinity;
    for (let i = 0; i < mesh.positions.length; i += 3) minX = Math.min(minX, mesh.positions[i]!);
    expect(minX).toBeCloseTo(100, 3);
    expect(() => resolvePartMesh(doc, engine, "nope")).toThrow(/not found/);
  });

  it("read lists assembly parts and opens one by name", async () => {
    const id = assemblySession();
    const server = await createServer(engine, { user: null });
    const client = new Client({ name: "t", version: "0" }, { capabilities: {} });
    const [c, s] = InMemoryTransport.createLinkedPair();
    await Promise.all([client.connect(c), server.connect(s)]);
    const call = async (args: Record<string, unknown>) =>
      out((await client.callTool({ name: "read", arguments: { document_id: id, ...args } })) as never);

    const listed = await call({});
    expect(listed.parts.map((p: { id: string }) => p.id)).toEqual(["tray1", "thigh1"]);
    const part = await call({ part_id: "thigh" });
    expect(part.part_id).toBe("thigh");
    expect(Object.values(part.nodes as Record<string, { op: { type: string } }>)[0]!.op.type).toBe("Cube");
  });
});

describe("set_parameters on a loon assembly", () => {
  // Sketch points have no binding path, so link_w gets no binding at all.
  const SKETCH_LINK = `[defparam link_w 30.0]
[defparam unused 7.0]
[let link [extrude 0 0 5 [sketch 0 0 0 1 0 0 0 1 0 #[[polygon #[0 0 link_w 0 link_w 10 0 10]]]]]]
[assembly #[[part "link" link "steel"]] #[[instance "l1" "link" 0 0 0]] #[] "l1"]`;

  async function session(): Promise<string> {
    const id = assemblySession(SKETCH_LINK);
    expect(Object.keys(getSession(id).bindings ?? {})).toHaveLength(0);
    return id;
  }
  const volume = (id: string) => computeInspection(getSession(id), engine).volume_mm3;

  it("rebuilds from source when the parameter has no binding, repeatedly", async () => {
    const id = await session();
    expect(volume(id)).toBeCloseTo(1500, 0);

    const first = out(await setParameters({ document_id: id, parameters: { link_w: 40 } }, engine));
    expect(first.no_effect).toBeUndefined();
    expect(first.reevaluated_from_source).toEqual(["link_w"]);
    expect(volume(id)).toBeCloseTo(2000, 0);

    // The dispatcher marks set_parameters as divergence; that must not lock out the next call.
    markSourceDiverged(id, "set_parameters");
    await setParameters({ document_id: id, parameters: { link_w: 50 } }, engine);
    expect(volume(id)).toBeCloseTo(2500, 0);
  });

  it("says why when a declared parameter really drives nothing", async () => {
    const id = await session();
    const res = out(await setParameters({ document_id: id, parameters: { unused: 9 } }, engine));
    expect(res.no_effect[0].reason).toContain("changes no geometry");
  });

  it("explains itself when direct edits prevent re-running the source", async () => {
    const id = await session();
    markSourceDiverged(id, "update");
    const res = out(await setParameters({ document_id: id, parameters: { link_w: 40 } }, engine));
    expect(res.no_effect[0].reason).toContain("can't be re-run");
    expect(volume(id)).toBeCloseTo(1500, 0);
  });

  it("keeps a board carried on the session through a rebuild", async () => {
    const id = await boardSession();
    await createCadLoon.handler({ document_id: id, source: SKETCH_LINK }, ctx);
    const doc = getSession(id);
    delete doc.source!.diverged_by;
    doc.source!.diverged = false;
    await setParameters({ document_id: id, parameters: { link_w: 40 } }, engine);
    expect(getPcbNodeIds(getSession(id))).toHaveLength(1);
    expect(getSession(id).schematic).toBeDefined();
  });
});

describe("load_document path", () => {
  const fileStore = { scope: "capability" } as unknown as SessionStore;

  function vcadFile(): { dir: string; path: string } {
    const dir = mkdtempSync(join(tmpdir(), "vcad-load-"));
    const path = join(dir, "part.loon");
    writeFileSync(path, "[cube 10 10 10]");
    return { dir, path };
  }

  it("works on a local server backed by the file session store", async () => {
    const { dir, path } = vcadFile();
    const res = await loadDocument({ path }, fileStore, engine);
    rmSync(dir, { recursive: true });
    expect(res.isError).toBeFalsy();
    expect(out(res).document_id).toBeTruthy();
  });

  it("still refuses on a hosted server", async () => {
    const { dir, path } = vcadFile();
    process.env.VCAD_MCP_REMOTE = "1";
    const res = await loadDocument({ path }, fileStore, engine);
    rmSync(dir, { recursive: true });
    expect(res.isError).toBe(true);
    expect(res.content[0]!.text).toContain("local/stdio only");
  });
});

describe("create_robot_env without end effectors", () => {
  it("defaults to none, and the schema doesn't require them", async () => {
    const doc = engine.evalLoonDocument(
      `[assembly #[[part "a" [cube 10 10 10] "steel"]] #[[instance "a1" "a" 0 0 0]] #[] "a1"]`,
      {},
      {},
    )!;
    const res = await createRobotEnv({ document_id: registerSession(doc) });
    expect(res.content[0]!.text).not.toContain("reading 'length'");
    expect(res.isError).toBeFalsy();
    expect(out(res).end_effector_ids).toEqual([]);
    expect(createRobotEnvSchema.required ?? []).not.toContain("end_effector_ids");
    expect(batchCreateEnvsSchema.required).toEqual(["n_envs"]);
  });
});

describe("create_schematic without coordinates", () => {
  it("places components on a grid", async () => {
    const pins = resistor("R", 0, ["A", "B"]).pins;
    const res = await createSchematic({
      components: [
        { ref: "R1", value: "1k", footprint: "Resistor_SMD:R_0805", pins },
        { ref: "R2", value: "1k", footprint: "Resistor_SMD:R_0805", pins },
      ],
    });
    const sheet = getSession(out(res).document_id).schematic!;
    expect(sheet.components.map((c) => c.position)).toEqual([
      { x: 0, y: 0 },
      { x: 25.4, y: 0 },
    ]);
    const items = createSchematicSchema.properties.components.items as { required?: string[] };
    expect(items.required ?? []).not.toContain("x");
  });

  it("still rejects a non-numeric coordinate", async () => {
    await expect(
      createSchematic({ components: [{ ...resistor("R1", 0, ["A", "B"]), x: "left" }] }),
    ).rejects.toThrow(/finite number/);
  });
});

describe("export_gerber output_dir", () => {
  it("writes the files to an absolute directory", async () => {
    const id = await boardSession();
    const dir = join(mkdtempSync(join(tmpdir(), "vcad-gerber-")), "fab");
    const res = out(await exportGerber({ document_id: id, output_dir: dir, require_clean_drc: false }));
    expect(res.success).toBe(true);
    expect(res.files.length).toBeGreaterThan(0);
    for (const f of res.files) expect(statSync(join(dir, f.name)).size).toBeGreaterThan(0);
    rmSync(dir, { recursive: true });
  });

  it("refuses on a hosted server without touching the disk", async () => {
    const id = await boardSession();
    const dir = join(tmpdir(), `vcad-gerber-hosted-${Date.now()}`);
    process.env.VCAD_MCP_REMOTE = "1";
    const res = await exportGerber({ document_id: id, output_dir: dir, require_clean_drc: false });
    expect(res.isError).toBe(true);
    expect(existsSync(dir)).toBe(false);
  });
});

describe("verify_receipt through the tool surface", () => {
  it("re-verifies clearance claims with the connection's engine", async () => {
    const doc = engine.evalLoonDocument(
      `[assembly #[[part "a" [cube 10 10 10] "steel"] [part "b" [cube 10 10 10] "steel"]]
        #[[instance "a1" "a" 0 0 0] [instance "b1" "b" 20 0 0]] #[] "a1"]`,
      {},
      {},
    )!;
    const id = registerSession(doc);
    const checked = await checkClearance(
      { document_id: id, group_a: ["a1"], group_b: ["b1"], min_mm: 1, label: "gap" },
      engine,
    );
    expect(checked.isError).toBeUndefined();
    const built = out(await buildReceipt({ document_id: id }, engine));

    const verify = ecadToolDefs.find((t) => t.name === "verify_receipt")!;
    const res = await verify.handler({ document_id: id, receipt: built }, ctx);
    expect(res.content[0]!.text).not.toContain("needs the kernel engine");
    expect(out(res as never).status).toBe("Holds");
  });
});
