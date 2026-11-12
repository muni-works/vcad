import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";

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

// Cloudflare Web Analytics (cookieless, no consent banner; see docs/privacy.md).
// Injected into every page only when CF_BEACON_TOKEN is set at build time —
// the token comes from the Web Analytics site Cam creates in Cloudflare, set
// as a build env var on the vcad-site Pages project. Without it, no beacon.
const cfToken = (process.env.CF_BEACON_TOKEN || "").trim();
const cfAnalytics = {
  name: "cf-analytics",
  transformIndexHtml() {
    if (!/^[A-Za-z0-9]+$/.test(cfToken)) return [];
    return [{
      tag: "script",
      attrs: { defer: true, src: "https://static.cloudflareinsights.com/beacon.min.js", "data-cf-beacon": JSON.stringify({ token: cfToken }) },
      injectTo: "body",
    }];
  },
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

// /export — the read-only off-ramp (sign in, download your cloud documents).
// Built standalone from packages/app (vite.export.config.ts → dist-export,
// base /export/) and shipped under /export/ so this site can serve all of
// vcad.io. A production build refuses to ship without it.
const EXPORT_DIR = page("../packages/app/dist-export");
const walk = (d) => readdirSync(d, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? walk(join(d, e.name)) : [join(d, e.name)]));
const exportPage = {
  name: "export-page",
  apply: "build",
  generateBundle() {
    if (!existsSync(join(EXPORT_DIR, "index.html"))) {
      if (process.env.VCAD_SKIP_EXPORT) return;
      this.error("packages/app/dist-export missing — run `npm run build:export -w @vcad/app` (or set VCAD_SKIP_EXPORT=1)");
    }
    for (const f of walk(EXPORT_DIR)) {
      this.emitFile({ type: "asset", fileName: `export/${relative(EXPORT_DIR, f)}`, source: readFileSync(f) });
    }
  },
};

export default defineConfig({
  plugins: [siteKernel, appcast, exportPage, cfAnalytics],
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
