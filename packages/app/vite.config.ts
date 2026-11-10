/// <reference types="vitest" />
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import basicSsl from "@vitejs/plugin-basic-ssl";
import tailwindcss from "@tailwindcss/vite";
import wasm from "vite-plugin-wasm";
import topLevelAwait from "vite-plugin-top-level-await";
import { VitePWA } from "vite-plugin-pwa";
import { fileURLToPath } from "url";
import { dirname, resolve } from "path";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

/**
 * esbuild plugin: Disables R3F's reconciler profiling marks in dev mode.
 *
 * React 19's dev reconciler runs expensive profiling (prop serialization + performance.measure)
 * for every fiber on every render, costing 10-15s on R3F scenes. The profiling is gated by a
 * `Me` flag that checks for `console.timeStamp` and `performance.measure` support — always true
 * in modern browsers. This patches the flag to `false`, skipping profiling while keeping all
 * other dev reconciler functionality (warnings, getOwner, etc.) intact.
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
function r3fDisableProfilingPlugin(): any {
  return {
    name: "r3f-disable-profiling",
    setup(build: any) {
      build.onLoad(
        { filter: /events-.*\.esm\.js$/ },
        async (args: any) => {
          if (!args.path.includes("@react-three/fiber")) return;
          const fs = await import("fs");
          let contents = await fs.promises.readFile(args.path, "utf8");
          // Disable the profiling flag: Me = typeof console < "u" && ... → Me = false
          const pattern = /(\w+)\s*=\s*typeof console\s*<\s*"u"\s*&&\s*typeof console\.timeStamp\s*==\s*"function"\s*&&\s*typeof performance\s*<\s*"u"\s*&&\s*typeof performance\.measure\s*==\s*"function"/;
          if (pattern.test(contents)) {
            contents = contents.replace(pattern, "$1 = false");
            return { contents, loader: "js" };
          }
        },
      );
    },
  };
}

/**
 * Injects a `<link rel="preload">` for the kernel WASM into index.html at
 * build time. The browser then starts the ~multi-MB fetch as soon as the
 * HTML streams in — well before main.tsx parses — overlapping it with
 * JS parse and React's first render. Build-only: in dev, the wasm is
 * served from an outside-root path Vite rewrites at runtime, so the
 * savings aren't worth the extra moving part.
 */
function preloadKernelWasmPlugin(): Plugin {
  return {
    name: "preload-kernel-wasm",
    apply: "build",
    transformIndexHtml: {
      order: "post",
      handler(html, ctx) {
        if (!ctx.bundle) return html;
        const wasmAsset = Object.keys(ctx.bundle).find((n) =>
          /vcad_kernel_wasm.*\.wasm$/.test(n),
        );
        if (!wasmAsset) return html;
        const link = `<link rel="preload" href="/${wasmAsset}" as="fetch" type="application/wasm" crossorigin>`;
        return html.replace("</head>", `    ${link}\n  </head>`);
      },
    },
  };
}

export default defineConfig((_env) => {
  return {
    server: {
      // CLAUDE_PREVIEW: served behind a cloud reverse proxy (Claude Code on
      // the web) — bind all hostnames since the inbound Host header is
      // generated per-session. Local dev keeps the existing "mew" entry.
      allowedHosts: process.env.CLAUDE_PREVIEW === "1" ? true : ["mew"],
    },
    envDir: "../../",
    define: {
      __APP_VERSION__: JSON.stringify(process.env.npm_package_version || "0.0.0"),
      __BUILD_TIME__: JSON.stringify(new Date().toISOString()),
    },
    plugins: [
      // basicSsl produces a self-signed cert for local HTTPS dev. Skip it under
      // CLAUDE_PREVIEW (cloud proxy terminates TLS upstream and rejects
      // self-signed certs).
      process.env.CLAUDE_PREVIEW !== "1" && basicSsl(),
      preloadKernelWasmPlugin(),
      react(),
      tailwindcss(),
      wasm(),
      topLevelAwait(),
      VitePWA({
        registerType: "prompt",
        devOptions: {
          enabled: false,
        },
        includeAssets: ["fonts/**/*", "assets/**/*"],
        manifest: false,
        workbox: {
          // wasm is intentionally excluded from precache: the 10.6 MB kernel
          // wasm is fetched explicitly during boot (see bootstrap.ts) and the
          // browser caches it like any other asset. Precaching duplicated the
          // download with the runtime fetch, freezing the main thread.
          globPatterns: ["**/*.{js,css,html,woff,woff2,otf}"],
          maximumFileSizeToCacheInBytes: 12 * 1024 * 1024,
          runtimeCaching: [
            {
              urlPattern: ({ request, url }) =>
                request.destination === "" && url.pathname.endsWith(".wasm"),
              handler: "CacheFirst",
              options: { cacheName: "wasm-cache", expiration: { maxEntries: 5, maxAgeSeconds: 60 * 60 * 24 * 30 } },
            },
            {
              urlPattern: /\.(woff|woff2|otf|ttf)$/,
              handler: "CacheFirst",
              options: { cacheName: "font-cache", expiration: { maxEntries: 20, maxAgeSeconds: 60 * 60 * 24 * 365 } },
            },
            {
              urlPattern: /\.(png|jpg|jpeg|svg|gif|webp)$/,
              handler: "CacheFirst",
              options: { cacheName: "image-cache", expiration: { maxEntries: 50, maxAgeSeconds: 60 * 60 * 24 * 30 } },
            },
          ],
        },
      }),
    ],
    resolve: {
      alias: {
        "@": resolve(__dirname, "./src"),
        // Force a single resolution of kernel-wasm to prevent double-instantiation.
        // Without this, `@vcad/kernel-wasm` and `vcad-kernel-wasm` (package's internal name)
        // can resolve to different module instances, each with its own WASM memory,
        // causing "Out of bounds memory access" when pointers cross instances.
        "@vcad/kernel-wasm": resolve(__dirname, "../kernel-wasm/vcad_kernel_wasm.js"),
        "vcad-kernel-wasm": resolve(__dirname, "../kernel-wasm/vcad_kernel_wasm.js"),
      },
      // Force a single React instance across the workspace. Without this,
      // workspace packages (@vcad/core, etc.) can resolve their own
      // React copies, which throws "null is not an object (evaluating
      // 'resolveDispatcher().useState')" when components from different
      // packages mount together. react/jsx-runtime is dedup'd separately
      // because vite's optimizer treats it as a distinct import.
      dedupe: [
        "@vcad/kernel-wasm",
        "vcad-kernel-wasm",
        "react",
        "react-dom",
        "react/jsx-runtime",
        "react/jsx-dev-runtime",
      ],
    },
    worker: {
      format: "es",
      plugins: () => [wasm(), topLevelAwait()],
    },
    build: {
      // TEMPORARY: source maps in production so the minified React #185
      // ("Maximum update depth exceeded") stack on vcad.io resolves to a
      // real component. Revert this once we've identified and fixed the
      // looping component — source maps add ~10 MB to the deploy.
      sourcemap: true,
      rollupOptions: {
        output: {
          manualChunks: {
            three: ["three"],
            "three-fiber": ["@react-three/fiber"],
            "three-drei": ["@react-three/drei"],
            "three-postprocessing": ["@react-three/postprocessing"],
          },
        },
      },
    },
    optimizeDeps: {
      exclude: ["@vcad/kernel-wasm", "@vcad/engine"],
      // Crawl the entire src/ tree (not just the html entry + statically
      // imported modules) so deps that only appear inside lazy/dynamic chunks
      // — e.g. the phosphor-icons used inside FeatureTree — get into the
      // initial prebundle. Without this, vite re-runs optimizeDeps the first
      // time a lazy chunk imports a previously-unseen icon, invalidating the
      // prebundle hash and 504'ing every chunk that's already in flight.
      entries: ["./index.html", "./src/**/*.{ts,tsx}"],
      // Force-include the AI Elements / streamdown stack in the prebundle so
      // they all use the deduped React instance. Without this, vite can lazily
      // optimize them on first import with a stale React snapshot, triggering
      // the dispatcher null error.
      include: [
        "react",
        "react-dom",
        "react-dom/client",
        "react/jsx-runtime",
        "streamdown",
        "use-stick-to-bottom",
        "lucide-react",
      ],
      esbuildOptions: {
        plugins: [r3fDisableProfilingPlugin()],
      },
    },
    test: {
      environment: "happy-dom",
      setupFiles: ["./src/test/setup.ts"],
      include: ["src/**/*.test.{ts,tsx}"],
    },
  };
});
