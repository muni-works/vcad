#!/usr/bin/env node
// Re-place and re-route ice-viz through the vcad MCP server, then export the fab bundle.
//   node relayout.mjs [in.vcad] [outDir] [W] [H]      (defaults: ../ice-viz.vcad ./out 46 40.5)
// Writes <outDir>/ice-viz.vcad, gerbers/drill/BOM/pick-and-place, ice-viz.kicad_pcb, top.png.
// The floorplan and the hand pre-routes live in layout.mjs; everything else is route_nets.
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { layout } from "./layout.mjs";

const here = path.dirname(new URL(import.meta.url).pathname);
const [inPath = path.join(here, "../ice-viz.vcad"), outArg = "out", W = "46", H = "40.5"] = process.argv.slice(2);
const out = path.resolve(outArg);
fs.mkdirSync(out, { recursive: true });

// minimal stdio MCP client
const srv = spawn("npx", ["-y", "@vcad/mcp"], { stdio: ["pipe", "pipe", "inherit"], env: { ...process.env, VCAD_MCP_STATE_DIR: out, VCAD_MCP_SESSION_DIR: path.join(out, ".sessions") } });
let seq = 0, buf = "";
const pending = new Map();
srv.stdout.on("data", (d) => {
  buf += d;
  for (let i; (i = buf.indexOf("\n")) >= 0; buf = buf.slice(i + 1)) {
    let m; try { m = JSON.parse(buf.slice(0, i)); } catch { continue; }
    if (pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); }
  }
});
const req = (method, params) => new Promise((res) => { const id = ++seq; pending.set(id, res); srv.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n"); });
const call = async (name, args) => {
  const r = (await req("tools/call", { name, arguments: args })).result;
  const text = (r.content || []).filter((c) => c.type === "text").map((c) => c.text).join("\n");
  let json = r.structuredContent; if (!json) try { json = JSON.parse(text); } catch {}
  return { json, text, content: r.content || [] };
};
await req("initialize", { protocolVersion: "2025-03-26", capabilities: {}, clientInfo: { name: "ice-viz-relayout", version: "1" } });
srv.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");

// Every part moves, so all existing copper is stale: start from the document with it stripped.
const doc = JSON.parse(fs.readFileSync(inPath));
const board = Object.values(doc.nodes)[0].op.board;
board.traces = []; board.vias = []; board.zones = [];
const id = (await call("open_document", { initial: doc })).json.document_id;

const L = layout(+W, +H);
await call("set_placement", { document_id: id, placements: L.P });
await call("set_board_outline", { document_id: id, board_width: +W, board_height: +H });
const pd = (await call("set_placement", { document_id: id, placements: [L.P[0]] })).json.placement_drc;
console.log("placement clean:", pd.clean, "courtyard overlaps:", pd.courtyard_overlaps);

await call("add_zone", { document_id: id, net: "GND", layer: "BCu", fill_board: true });
for (const t of L.T) await call("add_trace", { document_id: id, ...t });
const route = (await call("route_nets", { document_id: id })).json;
console.log("route:", route.nets_routed, "nets, open:", route.open_nets ?? []);

// Conditional patches: keep one only if it closes something and breaks nothing.
const openNets = async () => new Set(((await call("run_drc", { document_id: id, detail: "full" })).json.details || [])
  .filter((v) => /Unconnected|Island/.test(v.rule)).map((v) => /'([^']+)'/.exec(v.message)?.[1]));
const open = await openNets();
for (const t of L.X.filter((t) => open.has(t.net))) {
  const dd = (await call("add_trace", { document_id: id, ...t })).json?.drc_delta ?? {};
  if (!(dd.resolved > 0 && dd.introduced === 0)) await call("undo", { document_id: id });
}

const drc = (await call("run_drc", { document_id: id })).json;
const desc = (await call("describe_pcb", { document_id: id })).json;
const fab = (await call("validate_for_fab", { document_id: id })).json;
console.log(`board ${W}x${H} mm | segments ${desc.traces.segments} | vias ${desc.vias.count} | DRC ${drc.errors} errors, ${drc.warnings} warnings | fab: ${fab.verdict}`);
if (drc.errors || !fab.ready) { console.error("NOT CLEAN — nothing exported", drc.byRule); srv.kill(); process.exit(1); }

await call("export_gerber", { document_id: id, output_dir: out });
const k = (await call("export_kicad", { document_id: id, filename: "ice-viz.kicad_pcb" })).json;
fs.writeFileSync(path.join(out, "ice-viz.kicad_pcb"), (k.export_kicad ?? k).content);
await call("save_document", { document_id: id, name: "ice-viz" });
const img = (await call("render_pcb", { document_id: id, width_px: 2048, layers: ["B.Cu", "F.Cu", "F.SilkS", "Edge_Cuts"] })).content.find((c) => c.type === "image");
if (img) fs.writeFileSync(path.join(out, "top.png"), Buffer.from(img.data, "base64"));
fs.rmSync(path.join(out, ".sessions"), { recursive: true, force: true });
srv.kill();
