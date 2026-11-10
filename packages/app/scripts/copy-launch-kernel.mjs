// Copy the kernel WASM next to the launch page (public/launch/kernel/) so the
// hero can run the real kernel in a worker, same-origin under vcad.io's CSP.
// Copied at build time, never committed: the wasm-pack output is 35 MB and has
// a single writer on main (see CLAUDE.md, "Never commit the generated
// packages/kernel-wasm/vcad_kernel_wasm* artifacts from a feature branch").
import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const src = join(here, "../../kernel-wasm");
const dest = join(here, "../public/launch/kernel");
const files = ["vcad_kernel_wasm.js", "vcad_kernel_wasm_bg.wasm"];
if (!files.every((f) => existsSync(join(src, f)))) {
  console.warn("[launch] kernel-wasm artifacts missing; the launch hero will show its fallback");
  process.exit(0);
}
mkdirSync(dest, { recursive: true });
for (const f of files) copyFileSync(join(src, f), join(dest, f));
console.log(`[launch] kernel copied to ${dest}`);
