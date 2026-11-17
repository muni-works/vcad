#!/usr/bin/env node
// Regenerate the stator v3 board outputs — pipeline.py steps B1–B4 only, no
// HTTP bridge, no quotes — by driving a vcad MCP server over stdio.
//
//   node examples/pcb-motor/scripts/stator-board.mjs [path/to/mcp/dist/index.js]
//
// Writes, relative to examples/pcb-motor/: stator-v3.vcad,
// fab/stator-v3-gerbers/, fab/stator-v3-board.png. Exits 1 (writing nothing
// further) when a gate fails: a feed the realizer could not route, DRC != 0,
// a DFM error, or a coil that carries no current.
import { spawn } from "node:child_process";
import { writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const EX = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SERVER = process.argv[2] ?? resolve(EX, "../../packages/mcp/dist/index.js");
const child = spawn("node", [SERVER], {
  cwd: EX,
  stdio: ["pipe", "pipe", "inherit"],
  env: { ...process.env, VCAD_MCP_DISK_SESSIONS: "0", VCAD_MCP_STATE_DIR: EX },
});
let buf = "";
const pending = new Map();
let nextId = 1;
child.stdout.on("data", (d) => {
  buf += d;
  let nl;
  while ((nl = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, nl);
    buf = buf.slice(nl + 1);
    let m;
    try { m = JSON.parse(line); } catch { continue; }
    if (pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); }
  }
});
const rpc = (method, params) => new Promise((r) => {
  const id = nextId++;
  pending.set(id, r);
  child.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
});
await rpc("initialize", { protocolVersion: "2024-11-05", capabilities: {}, clientInfo: { name: "stator-board", version: "1" } });
child.stdin.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }) + "\n");

const failures = [];
async function call(name, args, { raw = false } = {}) {
  const m = await rpc("tools/call", { name, arguments: args });
  const content = m.result?.content ?? [];
  if (raw) return { content, isError: !!m.result?.isError };
  const text = content.find((c) => c.type === "text")?.text ?? JSON.stringify(m.error);
  if (m.result?.isError) console.log(`  !! ${name}: ${text.slice(0, 300)}`);
  try { return JSON.parse(text); } catch { return { __raw__: text, __error__: !!m.result?.isError }; }
}
const check = (label, ok, detail = "") => {
  console.log(`  ${ok ? "PASS" : "FAIL"}  ${label}  ${detail}`);
  if (!ok) failures.push(label);
};
const done = (code) => { child.kill(); process.exit(code); };

// B1 schematic + board + rules
const sch = await call("create_schematic", {
  title: "PCB Stator v3 — 9s/6p ferrite-PM axial",
  components: [{ ref: "J1", footprint: "JST_XH_4", value: "B4B-XH-A", x: 10, y: 10, pins: [
    { number: "1", name: "PHA", type: "Passive" }, { number: "2", name: "PHB", type: "Passive" },
    { number: "3", name: "PHC", type: "Passive" }, { number: "4", name: "WIND_N", type: "Passive" }] }],
  nets: { PHA: ["J1.1"], PHB: ["J1.2"], PHC: ["J1.3"], WIND_N: ["J1.4"] },
});
const DOC = sch.document_id;
await call("place_components", { document_id: DOC, board_shape: { type: "circle", outer_diameter: 70, inner_diameter: 10 },
  board_thickness: 1.6, strategy: "radial", radial_radius: 32, radial_start_angle_deg: 100 });
await call("set_stackup", { document_id: DOC, copper_oz: 2 });
await call("set_design_rules", { document_id: DOC, clearance: 0.2, track_width: 0.25, via_drill: 0.3,
  via_diameter: 0.6, edge_clearance: 0.3, min_drill: 0.3, min_annular_ring: 0.13 });
await call("set_placement", { document_id: DOC, placements: [{ ref: "J1", x: 29.44, y: 66.51, rotation: 190 }] });

// B2 winding — every feed from the realizer, no stitches (see pipeline.py)
const WINDING = { slots: 9, poles: 6, phases: 3, center: { x: 35, y: 35 }, pitch_radius: 22.5,
  inner_radius: 2.6, outer_radius: 7.2, trace_width: 0.25, clearance: 0.2, turns_per_coil: 10,
  connection: "wye", copper_layer: "FCu", return_layer: "BCu", neutral_net: "WIND_N",
  phase_nets: ["PHA", "PHB", "PHC"] };
const w = await call("add_motor_winding", { document_id: DOC, ...WINDING });
check("B2 all feeds routed by the realizer", !w.feeds_unrouted && w.success === true,
  `routed=${w.feeds_routed} unrouted=${w.feeds_unrouted ?? "-"} ${w.errors ? JSON.stringify(w.errors) : ""}`);

// B3 bore-mount outline
const ng = (cx, cy, r, n) => Array.from({ length: n }, (_, i) => ({
  x: +(cx + r * Math.cos((2 * Math.PI * i) / n)).toFixed(4), y: +(cy + r * Math.sin((2 * Math.PI * i) / n)).toFixed(4) }));
const P = (r, d) => [35 + r * Math.cos((d * Math.PI) / 180), 35 + r * Math.sin((d * Math.PI) / 180)];
await call("set_board_outline", { document_id: DOC, thickness: 1.6, outline: {
  vertices: ng(35, 35, 35, 64), cutouts: [ng(35, 35, 5.0, 32), ...[60, 180, 300].map((a) => ng(...P(8, a), 1.7, 16))] } });

// B4 gates
const drc = await call("run_drc", { document_id: DOC });
check("B4 DRC clean", drc.violations === 0, `violations=${drc.violations} ${JSON.stringify(drc.byRule ?? {})}`);
const wcRes = await call("check_winding", { document_id: DOC, slots: 9, poles: 6, phases: 3, center: WINDING.center,
  pitch_radius: 22.5, outer_radius: 7.2, turns_per_coil: 10, copper_layer: "FCu",
  phase_nets: WINDING.phase_nets, neutral_net: "WIND_N" }, { raw: true });
const wc = JSON.parse(wcRes.content[0].text);
for (const c of wc.coils ?? []) {
  console.log(`    coil ${c.net} @ (${c.center.x.toFixed(2)},${c.center.y.toFixed(2)}): ${c.ampere_turns_per_amp.toFixed(2)} AT/A${c.ok ? "" : "  <- " + c.problem}`);
}
for (const p of wc.phases ?? []) console.log(`    phase ${p.net}: ${p.resistance_ohm} ohm, dead copper ${p.dead_copper_mm} mm`);
check("B4 winding check: every coil carries current", wc.pass === true, (wc.failures ?? []).join("; "));
const dfm = await call("dfm_check", { document_id: DOC, process: "pcb_jlcpcb" });
const hard = (dfm.rules ?? []).filter((r) => r.applicable && !r.passed && r.severity === "error");
check("B4 DFM jlcpcb no errors", hard.length === 0, hard.map((r) => `${r.rule}x${r.violations}`).join("; ") || "warnings only");
if (failures.length) { console.log(`\n${failures.length} gate(s) failed — nothing written.`); done(1); }

await call("export_gerber", { document_id: DOC, output_dir: resolve(EX, "fab/stator-v3-gerbers") });
const img = await call("render_pcb", { document_id: DOC, layers: ["F.Cu", "B.Cu", "F.SilkS", "Edge_Cuts"] }, { raw: true });
const png = img.content.find((c) => c.type === "image");
if (png) writeFileSync(resolve(EX, "fab/stator-v3-board.png"), Buffer.from(png.data, "base64"));
const saved = await call("save_document", { document_id: DOC, name: "stator-v3" });
const rcpt = await call("build_receipt", { document_id: DOC });
console.log(`  saved ${saved.path ?? JSON.stringify(saved).slice(0, 120)}`);
console.log(`  receipt board_hash ${rcpt.receipt?.board_hash ?? rcpt.board_hash}`);
done(0);
