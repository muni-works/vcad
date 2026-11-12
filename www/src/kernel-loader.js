// Shared kernel loader for the site's live demos.
//
// - Loads the slim site kernel (crates/vcad-site-wasm, built by
//   scripts/build-site-kernel.sh) — falls back to the full checked-in
//   packages/kernel-wasm bundle in dev when the slim build is absent
//   (see the `@site-kernel` alias in vite.config.js).
// - Streams: the fetch body is counted as it arrives (real progress) and
//   handed to WebAssembly.instantiateStreaming, so compile overlaps download.
// - One memoized instance per page; every demo shares it.
// - `autoloadOK()` says whether to fetch without asking: never on Save-Data,
//   2g/3g, or a measured downlink under ~1.5 Mbps. Callers show a "Load
//   interactive demo" button instead (see attachGate).

import init, * as kernel from "@site-kernel";
import wasmUrl from "@site-kernel-wasm?url";
import { KERNEL_BYTES, KERNEL_WIRE_BYTES } from "@site-kernel-meta";

let promise = null;
const listeners = new Set();

/** Raw (decoded) size of the kernel module in bytes. */
export const kernelBytes = KERNEL_BYTES;

export function autoloadOK() {
  const c = navigator.connection;
  if (!c) return true; // Safari/Firefox: no signal, assume fine
  if (c.saveData) return false;
  if (/(^|-)?[23]g$/.test(c.effectiveType || "")) return false;
  if (typeof c.downlink === "number" && c.downlink > 0 && c.downlink < 1.5) return false;
  return true;
}

/** Subscribe to load progress: fn(fraction 0..1). Returns an unsubscribe. */
export function onKernelProgress(fn) {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

const emit = (f) => listeners.forEach((fn) => { try { fn(f); } catch {} });

/** Load (once) and return the kernel module namespace. */
export function loadKernel() {
  if (!promise) {
    promise = (async () => {
      const res = await fetch(wasmUrl);
      if (!res.ok || !res.body) throw new Error(`kernel fetch failed: ${res.status}`);
      let got = 0;
      const total = KERNEL_BYTES || Number(res.headers.get("content-length")) || 0;
      const counted = res.body.pipeThrough(new TransformStream({
        transform(chunk, ctl) {
          got += chunk.byteLength;
          if (total) emit(Math.min(0.99, got / total));
          ctl.enqueue(chunk);
        },
      }));
      // a fresh Response keeps application/wasm so init() streams-compiles
      const streamed = new Response(counted, { headers: { "content-type": "application/wasm" } });
      await init({ module_or_path: streamed });
      emit(1);
      return kernel;
    })();
    promise.catch(() => { promise = null; }); // allow a retry
  }
  return promise;
}

/**
 * Gate a demo behind the connection check. Calls `start()` right away when
 * autoloading is OK; otherwise puts a "Load interactive demo" button over
 * `host` and calls `start()` on tap. Either way a progress bar shows over
 * `host` while the kernel downloads, and is removed when it's ready.
 */
export function attachGate(host, start, { label = "Load interactive demo" } = {}) {
  if (host instanceof HTMLCanvasElement) {
    // canvases can't hold the overlay — wrap in a positioned box
    const wrap = document.createElement("div");
    wrap.className = "kgate-wrap";
    host.replaceWith(wrap);
    wrap.appendChild(host);
    host = wrap;
  }
  host.classList.add("kgate-host");
  const ui = document.createElement("div");
  ui.className = "kgate mono";
  host.appendChild(ui);
  const mb = (KERNEL_WIRE_BYTES / 1e6).toFixed(1); // compressed transfer

  const showProgress = () => {
    ui.innerHTML = `<span class="kgate-label">loading the kernel</span><span class="kgate-bar"><i></i></span>`;
    const bar = ui.querySelector("i");
    const off = onKernelProgress((f) => { bar.style.width = `${Math.round(f * 100)}%`; });
    return () => { off(); ui.remove(); host.classList.remove("kgate-host"); };
  };
  const go = () => {
    const done = showProgress();
    Promise.resolve(start())
      .then(done)
      .catch((e) => {
        console.warn("kernel demo unavailable:", e);
        ui.innerHTML = `<button type="button" class="kgate-btn">Retry loading the demo</button>`;
        ui.querySelector("button").onclick = go;
      });
  };
  if (autoloadOK()) { go(); return; }
  ui.innerHTML = `<button type="button" class="kgate-btn">${label} <span class="t-dim2">· ${mb} MB</span></button>`;
  ui.querySelector("button").onclick = go;
}
