#!/usr/bin/env node
// vcad 1.0 hero demo, scripted: "Make me a glockenspiel in C major I can
// print or mill." Drives the native Rust MCP server (vcad-mcp) over stdio
// exactly as an agent would, then hands the document to the fab dry runs.
//
//   cargo build --release -p vcad-mcp -p vcad-ffi --example instrument_fab
//   node examples/glockenspiel/hero/run.mjs [target-dir] [material]
//
// Writes examples/glockenspiel/hero/out/: glockenspiel.vcad, receipt.json,
// iso.png / top.png renders, transcript.json, and (via instrument_fab) the
// print + mill G-code. Nothing is sent to a printer or a mill.

import { spawn, execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const target = resolve(process.argv[2] ?? process.env.CARGO_TARGET_DIR ?? join(here, "../../../target"));
const material = process.argv[3] ?? "aluminium_6061";
const out = join(here, "out");
mkdirSync(out, { recursive: true });

const server = spawn(join(target, "release/vcad-mcp"),
  ["--headless", "--no-proxy", "--session-dir", mkdtempSync(join(tmpdir(), "vcad-hero-"))],
  { stdio: ["pipe", "pipe", "inherit"], env: { ...process.env, VCAD_MCP_EXPORT_DIR: out } });

let buf = "";
const waiting = new Map();
server.stdout.on("data", (d) => {
  buf += d;
  let i;
  while ((i = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, i).trim();
    buf = buf.slice(i + 1);
    if (!line) continue;
    const msg = JSON.parse(line);
    waiting.get(msg.id)?.(msg);
  }
});
let next = 1;
const rpc = (method, params) => new Promise((ok, fail) => {
  const id = next++;
  waiting.set(id, (m) => (m.error ? fail(new Error(JSON.stringify(m.error))) : ok(m.result)));
  server.stdin.write(JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n");
});
const transcript = [];
const call = async (name, args) => {
  const t0 = Date.now();
  const r = await rpc("tools/call", { name, arguments: args });
  transcript.push({ tool: name, args, ms: Date.now() - t0, result: r.structuredContent ?? null });
  if (r.isError) throw new Error(`${name}: ${r.content?.[0]?.text}`);
  return r;
};

await rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "hero-demo", version: "1" } });

console.log(`> Make me a glockenspiel in C major I can print or mill. (${material})`);
const design = (await call("design_instrument", { tonic: "C6", material })).structuredContent;
const id = design.document_id;
console.log(`design_instrument → ${id}, receipt ${design.verdict}`);
console.log("note   target Hz  predicted Hz   cents   length  undercut  f2/f1");
for (const b of design.bars) {
  console.log(`${b.note.padEnd(5)} ${b.target_hz.toFixed(2).padStart(9)} ${b.predicted_hz.toFixed(2).padStart(13)} ${b.cents_error.toFixed(3).padStart(7)}  ${b.length_mm.toFixed(1).padStart(6)}  ${b.undercut_depth_mm.toFixed(2).padStart(7)}  ${b.overtone_ratio.toFixed(3)}`);
}

for (const view of ["iso", "top"]) {
  const r = await call("render_view", { document_id: id, view, width_px: 1200 });
  const img = r.content.find((c) => c.type === "image");
  if (img) writeFileSync(join(out, `${view}.png`), Buffer.from(img.data, "base64"));
}
await call("export_cad", { document_id: id, filename: "glockenspiel.vcad" });
const receipt = (await call("instrument_receipt", { document_id: id })).structuredContent;
writeFileSync(join(out, "receipt.json"), JSON.stringify(receipt, null, 2));
writeFileSync(join(out, "transcript.json"), JSON.stringify(transcript, (k, v) => (k === "loon" ? `${v.length} chars` : v), 2));
server.stdin.end();

console.log(`\nfab dry runs (files only):`);
console.log(execFileSync(join(target, "release/examples/instrument_fab"), [join(out, "glockenspiel.vcad"), out], { encoding: "utf8" }));
