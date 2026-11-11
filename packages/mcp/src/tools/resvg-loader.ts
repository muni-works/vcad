/**
 * The SVG rasterizer: native `@resvg/resvg-js` when its platform binary
 * installed, otherwise the WASM build. The published bundle carries that WASM
 * beside itself (`resvg.wasm`), so PNG output doesn't depend on an optional
 * dependency having resolved. Both expose the same `Resvg` shape.
 */
import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/** The surface of `Resvg` that vcad's renderers use. */
type ResvgCtor = typeof import("@resvg/resvg-js").Resvg;
export type LoadedResvg = { Resvg: ResvgCtor } | { reason: string };

/** Where the WASM lives: beside the bundle, else in node_modules. */
function resvgWasmPath(): string | null {
  const here = dirname(fileURLToPath(import.meta.url));
  const bundled = join(here, "resvg.wasm");
  if (existsSync(bundled)) return bundled;
  try {
    return createRequire(import.meta.url).resolve("@resvg/resvg-wasm/index_bg.wasm");
  } catch {
    return null;
  }
}

async function loadWasm(): Promise<LoadedResvg> {
  const path = resvgWasmPath();
  if (!path) return { reason: "module-missing" };
  try {
    const wasm = await import("@resvg/resvg-wasm");
    await wasm.initWasm(readFileSync(path));
    return { Resvg: wasm.Resvg as unknown as ResvgCtor };
  } catch (e) {
    return { reason: `resvg import failed: ${e instanceof Error ? e.message : String(e)}` };
  }
}

let loading: Promise<LoadedResvg> | null = null;

async function load(): Promise<LoadedResvg> {
  try {
    const { Resvg } = await import("@resvg/resvg-js");
    return { Resvg };
  } catch {
    // A missing or unloadable native binary is exactly what the WASM is for.
    return loadWasm();
  }
}

/** The rasterizer, loaded once; `reason` is "module-missing" when neither build exists. */
export function loadResvg(): Promise<LoadedResvg> {
  return (loading ??= load());
}

/** Test seam: load the WASM build regardless of the native one. */
export function loadResvgWasmForTest(): Promise<LoadedResvg> {
  return loadWasm();
}
