import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";
import { existsSync, readFileSync, statSync } from "node:fs";

const page = (p) => fileURLToPath(new URL(p, import.meta.url));

// Live demos load the slim site kernel (crates/vcad-site-wasm, built into
// ./site-kernel by scripts/build-site-kernel.sh — a gitignored artifact).
// Dev falls back to the full checked-in ../packages/kernel-wasm bundle when
// the slim build is absent; a production build refuses to ship the 37 MB one.
const SLIM = page("./site-kernel/vcad_site_wasm.js");
const hasSlim = existsSync(SLIM);
const kernel = hasSlim
  ? { js: SLIM, wasm: page("./site-kernel/vcad_site_wasm_bg.wasm"), meta: page("./site-kernel/meta.json") }
  : { js: page("../packages/kernel-wasm/vcad_kernel_wasm.js"), wasm: page("../packages/kernel-wasm/vcad_kernel_wasm_bg.wasm") };
const meta = kernel.meta ? JSON.parse(readFileSync(kernel.meta, "utf8")) : { bytes: statSync(kernel.wasm).size };
const kernelBytes = meta.bytes;
const wireBytes = meta.brotli || Math.round(meta.bytes / 4);

const siteKernel = {
  name: "site-kernel",
  config(_, { command }) {
    if (command === "build" && !hasSlim && !process.env.VCAD_ALLOW_FULL_KERNEL) {
      throw new Error("www/site-kernel missing — run www/scripts/build-site-kernel.sh first (or set VCAD_ALLOW_FULL_KERNEL=1)");
    }
  },
  resolveId(id) { if (id === "@site-kernel-meta") return "\0site-kernel-meta"; },
  load(id) { if (id === "\0site-kernel-meta") return `export const KERNEL_BYTES = ${kernelBytes}; export const KERNEL_WIRE_BYTES = ${wireBytes};`; },
};

// The Mac app's Sparkle feed lives in packages/app/public/desktop (its single
// source, written by the release pipeline). Ship it at /desktop/appcast.xml so
// this site can take over vcad.io without breaking updates. _redirects only
// matches /desktop and /desktop/ exactly, so the feed is never redirected.
const appcast = {
  name: "appcast",
  generateBundle() {
    this.emitFile({ type: "asset", fileName: "desktop/appcast.xml", source: readFileSync(page("../packages/app/public/desktop/appcast.xml")) });
  },
};

export default defineConfig({
  plugins: [siteKernel, appcast],
  resolve: {
    alias: [
      { find: /^@site-kernel-wasm(?=\?|$)/, replacement: kernel.wasm },
      { find: /^@site-kernel$/, replacement: kernel.js },
    ],
  },
  server: {
    fs: { allow: [".."] },
  },
  build: {
    // keep the kernel out of the entry chunk — it is lazy-loaded post-LCP
    rollupOptions: {
      input: {
        main: page("./index.html"),
        design: page("./design/index.html"),
        prove: page("./prove/index.html"),
        agents: page("./agents/index.html"),
        kernel: page("./kernel/index.html"),
      },
      output: {
        manualChunks(id) {
          if (id.includes("kernel-wasm") || id.includes("site-kernel")) return "kernel";
        },
      },
    },
  },
});
