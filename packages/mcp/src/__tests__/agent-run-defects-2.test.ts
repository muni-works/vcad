import { describe, it, expect, beforeAll, afterEach, vi } from "vitest";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Engine } from "@vcad/engine";
import { getNodePcb, getPcbNodeIds } from "@vcad/core";
import type { Document, Pcb } from "@vcad/ir";
import {
  addVia,
  buildReceipt,
  createSchematic,
  exportGerber,
  placeComponents,
  validateForFab,
  verifyReceipt,
} from "../tools/ecad.js";
import { checkEnclosureFit } from "../tools/enclosure.js";
import { evaluateBoardsApart } from "../tools/document-solids.js";
import { generateGlbPreview } from "../tools/preview.js";
import { renderView, simplifyForRender } from "../tools/render.js";
import {
  documents,
  getDocumentTool,
  getSession,
  loadDocument,
  openDocument,
  saveDocument,
} from "../tools/session.js";
import { recordTriangles } from "../tools/session-core.js";
import { FileSessionStore } from "../session-store.js";

let engine: Engine;
beforeAll(async () => {
  engine = await Engine.init();
});
afterEach(() => vi.restoreAllMocks());

type Out = { content: Array<{ type: string; text?: string }>; isError?: boolean };
const out = (r: Out) => JSON.parse(r.content.find((c) => c.type === "text")!.text!);

async function board(): Promise<string> {
  const created = out(
    await createSchematic({
      components: [
        {
          ref: "R1",
          value: "1k",
          footprint: "Resistor_SMD:R_0805",
          x: 0,
          y: 0,
          pins: [
            { number: "1", name: "~", type: "Passive", x: -5, y: 0 },
            { number: "2", name: "~", type: "Passive", x: 5, y: 0 },
          ],
        },
      ],
      nets: {},
    }),
  );
  out(await placeComponents({ document_id: created.document_id, board_width: 30, board_height: 30 }));
  return created.document_id;
}

const pcbOf = (id: string): Pcb => {
  const doc = getSession(id);
  return getNodePcb(doc, getPcbNodeIds(doc)[0]!)!;
};

const cube = (id: number, x: number, y: number, z: number) => ({
  id,
  name: `n${id}`,
  op: { type: "Cube", size: { x, y, z } },
});

function doc(nodes: Array<Record<string, unknown>>, root: number): Document {
  return {
    version: "0.1",
    nodes: Object.fromEntries(nodes.map((n) => [String(n.id), n])),
    materials: {},
    part_materials: {},
    roots: [{ root, material: "default" }],
  } as unknown as Document;
}

/** 50×50×12 closed box with a 46×46×8 sealed void, floor at z=2. */
function closedCase(): Document {
  return doc(
    [
      cube(1, 50, 50, 12),
      cube(2, 46, 46, 8),
      { id: 3, name: "void", op: { type: "Translate", child: 2, offset: { x: 2, y: 2, z: 2 } } },
      { id: 4, name: "case", op: { type: "Difference", left: 1, right: 3 } },
    ],
    4,
  );
}

describe("check_enclosure_fit on a closed case", () => {
  it("finds the sealed cavity instead of calling it a solid block", async () => {
    const enclosure = out(openDocument({ initial: closedCase() })).document_id;
    const res = out(
      await checkEnclosureFit({ document_id: await board(), enclosure_document_id: enclosure }, engine),
    );
    expect(res.success).toBe(true);
    expect(res.cavity_basis).toBe("sealed-void");
    expect(res.note).toMatch(/not detected/);
    expect(res.cavity.hasLid).toBe(true);
    expect(res.cavity.floorZ).toBeCloseTo(2, 1);
    expect(res.cavity.ceilZ).toBeCloseTo(10, 1);
    expect(res.cavity.maxX - res.cavity.minX).toBeGreaterThan(45);
    expect(res.cavity.maxX - res.cavity.minX).toBeLessThanOrEqual(46.5);
  });

  it("still refuses a genuinely solid block", async () => {
    const block = out(openDocument({ initial: doc([cube(1, 50, 50, 12)], 1) })).document_id;
    const res = await checkEnclosureFit(
      { document_id: await board(), enclosure_document_id: block },
      engine,
    );
    expect(res.isError).toBe(true);
    expect(res.content[0]!.text).toMatch(/no interior cavity/i);
  });
});

describe("render_view on a document over the triangle budget", () => {
  it("drops board copper and says exactly what it left out", async () => {
    const id = await board();
    out(await addVia({ document_id: id, position: { x: 5, y: 5 }, net: "GND" }));
    recordTriangles(id, 144_350);
    const res = await renderView({ document_id: id }, engine);
    expect(res.isError).toBeUndefined();
    const text = res.content.find((c) => c.type === "text") as { text: string };
    const payload = JSON.parse(text.text);
    expect(payload.simplified.reason).toMatch(/144350/);
    expect(payload.simplified.omitted.join(" ")).toMatch(/1 via\(s\)/);
    expect(payload.simplified.omitted.join(" ")).toMatch(/render_pcb/);
  });

  it("decimates parts when dropping copper isn't enough, and renders the result", async () => {
    const n = 400;
    const positions: number[] = [];
    const indices: number[] = [];
    for (let j = 0; j <= n; j++) for (let i = 0; i <= n; i++) positions.push(i / 8, j / 8, Math.sin(i / 9) * Math.cos(j / 7));
    for (let j = 0; j < n; j++) {
      for (let i = 0; i < n; i++) {
        const a = j * (n + 1) + i;
        indices.push(a, a + 1, a + n + 1, a + 1, a + n + 2, a + n + 1);
      }
    }
    const dense = { positions: new Float32Array(positions), indices: new Uint32Array(indices) };
    const fake = { evaluate: () => ({ parts: [{ mesh: dense }], instances: [] }) } as unknown as Engine;
    const fitted = simplifyForRender(doc([cube(1, 50, 50, 1)], 1), 320_000, fake)!;
    expect(fitted.simplification.rendered_triangles).toBeLessThanOrEqual(120_000);
    expect(fitted.simplification.omitted.join(" ")).toMatch(/decimated from 320000/);
    expect(fitted.doc.nodes["1"]!.op.type).toBe("ImportedMesh");

    const id = out(openDocument({ initial: fitted.doc })).document_id;
    const rendered = await renderView({ document_id: id });
    expect(rendered.isError).toBeUndefined();
  });
});

describe("write cost on a board-heavy document", () => {
  it("the preview never evaluates a board it already drew", async () => {
    const id = await board();
    const spy = vi.spyOn(engine, "evaluate");
    const glb = await generateGlbPreview(getSession(id), engine);
    expect(glb).not.toBeNull();
    expect(spy).not.toHaveBeenCalled();
  });

  it("an edit elsewhere leaves the board's evaluation key unchanged", async () => {
    const id = await board();
    const current = getSession(id);
    const mixed: Document = {
      ...current,
      nodes: { ...current.nodes, "900": cube(900, 5, 5, 5) as never },
      roots: [...current.roots, { root: 900, material: "default" }],
    };
    const evaluate = engine.evaluate.bind(engine);
    const seen: string[] = [];
    vi.spyOn(engine, "evaluate").mockImplementation((d, o) => {
      seen.push(JSON.stringify(d));
      return evaluate(d, o);
    });
    const before = evaluateBoardsApart(mixed, engine);
    const boardKey = seen[0]!;
    expect(boardKey).toContain("PcbBoard");
    seen.length = 0;
    const edited: Document = {
      ...mixed,
      parameters: { link_w: { value: 7 } } as never,
      nodes: { ...mixed.nodes, "900": cube(900, 6, 5, 5) as never },
    };
    const after = evaluateBoardsApart(edited, engine);
    expect(seen[0]).toBe(boardKey);
    expect(after.parts).toHaveLength(before.parts.length);
  });
});

describe("export_gerber and validate_for_fab agree on blind vias", () => {
  async function blindBoard(): Promise<string> {
    const id = await board();
    pcbOf(id).vias.push({
      position: { x: 5, y: 5 },
      diameter: 0.6,
      drill: 0.3,
      startLayer: "FCu",
      endLayer: "In1Cu",
      net: "GND",
    } as Pcb["vias"][number]);
    return id;
  }

  it("blocks both by default, and require_clean_drc:false doesn't waive it", async () => {
    const id = await blindBoard();
    const verdict = out(await validateForFab({ document_id: id }));
    expect(verdict.ready).toBe(false);
    expect(verdict.unsupported_features[0].detail).toMatch(/layer-pair drill file/);
    expect(verdict.unsupported_features[0].detail).not.toMatch(/drills every via straight through/);
    const exported = out(await exportGerber({ document_id: id, require_clean_drc: false }));
    expect(exported.blocked).toBe(true);
    expect(exported.files).toBeUndefined();
    expect(exported.unsupported_features[0].count).toBe(1);
  });

  it("releases both together once an HDI process is confirmed", async () => {
    const id = await blindBoard();
    const verdict = out(await validateForFab({ document_id: id, allow_blind_vias: true }));
    expect(verdict.unsupported_features).toHaveLength(0);
    expect(verdict.process_requirements[0]).toMatch(/HDI/);
    const exported = out(
      await exportGerber({ document_id: id, require_clean_drc: false, allow_blind_vias: true }),
    );
    expect(exported.blocked).toBeUndefined();
    expect(exported.files.map((f: { name: string }) => f.name)).toContain("drill-F_Cu-In1_Cu.drl");
  });
});

describe("verify_receipt is bound to the document state", () => {
  it("Holds when nothing changed; Stale and says why after an unmeasured edit", async () => {
    const id = await board();
    const built = out(await buildReceipt({ document_id: id }, engine));
    expect(out(await verifyReceipt({ document_id: id, receipt: built }, engine)).status).toBe("Holds");

    (getSession(id) as Document & { parameters: unknown }).parameters = {
      link_w: { value: 12 },
    };
    const after = out(await verifyReceipt({ document_id: id, receipt: built }, engine));
    expect(after.status).toBe("Stale");
    expect(after.document_state.changed).toBe(true);
    expect(after.document_state.note).toMatch(/re-measured unchanged/);
  });

  it("flags a receipt that predates binding instead of implying it covers the document", async () => {
    const id = await board();
    const built = out(await buildReceipt({ document_id: id }, engine));
    delete built.document_state_sha256;
    delete built.unified.document_state_sha256;
    const res = out(await verifyReceipt({ document_id: id, receipt: built }, engine));
    expect(res.status).toBe("Holds");
    expect(res.document_state.bound).toBe(false);
  });
});

describe("local save_document and get_document", () => {
  const dirs: string[] = [];
  const temp = () => {
    const d = mkdtempSync(join(tmpdir(), "vcad-mcp-"));
    dirs.push(d);
    return d;
  };
  afterEach(() => {
    delete process.env.VCAD_MCP_STATE_DIR;
    delete process.env.MCP_MAX_INLINE_ARTIFACT_BYTES;
    for (const d of dirs.splice(0)) rmSync(d, { recursive: true, force: true });
  });

  it("writes <name>.vcad on the disk-session store and reopens it by name", async () => {
    const state = temp();
    process.env.VCAD_MCP_STATE_DIR = state;
    const store = new FileSessionStore(temp());
    const id = out(openDocument({ initial: doc([cube(1, 5, 5, 5)], 1) })).document_id;
    const saved = out(await saveDocument({ document_id: id, name: "bracket" }, store));
    expect(saved.path).toBe(join(state, "bracket.vcad"));
    expect(existsSync(saved.path)).toBe(true);
    const reopened = out(await loadDocument({ name: "bracket" }, store));
    expect(documents.has(reopened.document_id)).toBe(true);
  });

  it("gets a large document as a readable local path, not a relative URL", () => {
    const state = temp();
    process.env.VCAD_MCP_STATE_DIR = state;
    process.env.MCP_MAX_INLINE_ARTIFACT_BYTES = "16";
    const initial = doc([cube(1, 5, 5, 5)], 1);
    const id = out(openDocument({ initial })).document_id;
    const res = out(getDocumentTool({ document_id: id }));
    expect(res.artifact_url).toBeUndefined();
    expect(JSON.parse(readFileSync(res.path, "utf8")).nodes["1"].op.type).toBe("Cube");
  });
});
