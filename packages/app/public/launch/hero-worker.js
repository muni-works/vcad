// The launch hero's kernel: the real vcad BRep kernel, compiled to WASM,
// building the bracket on the page from its parameters. Runs in a worker so a
// 0.5 s boolean never blocks the page; the newest request always wins.
import init, { Solid } from "./kernel/vcad_kernel_wasm.js";

// The kernel narrates booleans on console.log; keep the console clean.
console.log = () => {};

let ready = false;
let pending = null;
let busy = false;
let last = null;

// A wall bracket: a drilled base plate and an upright with a bore.
// Every feature is a boolean on exact surfaces, the same as in the app.
function build(p) {
  const { w, d, t, h, n, hole, bore } = p;
  const pitch = w / (n + 1);
  const holes = Solid.cylinder(hole / 2, t * 3).translate(pitch, d * 0.42, -t).linearPattern(1, 0, 0, n, pitch);
  const plate = Solid.cube(w, d, t).difference(holes);
  const wall = Solid.cube(w, t, h).difference(
    Solid.cylinder(bore / 2, t * 3).rotate(90, 0, 0).translate(w / 2, t * 2, h * 0.58));
  return plate.union(wall.translate(0, d - t, 0));
}

function run() {
  if (!ready || busy || !pending) return;
  busy = true;
  const { id, params } = pending;
  pending = null;
  try {
    const t0 = performance.now();
    const solid = build(params);
    const t1 = performance.now();
    const mesh = solid.getMesh(48);
    const t2 = performance.now();
    const positions = new Float32Array(mesh.positions);
    const indices = new Uint32Array(mesh.indices);
    const kinds = new Uint8Array(mesh.faceKinds || []);
    const normals = new Float32Array(mesh.normals || []);
    const msg = {
      type: "mesh", id, positions, indices, kinds, normals,
      volume: solid.volume(), area: solid.surfaceArea(),
      solveMs: t1 - t0, meshMs: t2 - t1,
    };
    last?.free?.();
    last = solid;
    postMessage(msg, [positions.buffer, indices.buffer, kinds.buffer, normals.buffer]);
  } catch (e) {
    postMessage({ type: "error", id, message: String(e?.message || e) });
  }
  busy = false;
  run();
}

onmessage = (e) => {
  const m = e.data;
  if (m.type === "build") { pending = m; run(); }
  if (m.type === "step" && last) {
    const bytes = last.toStepBuffer();
    postMessage({ type: "step", bytes }, [bytes.buffer]);
  }
};

const t0 = performance.now();
init({ module_or_path: new URL("./kernel/vcad_kernel_wasm_bg.wasm", import.meta.url) })
  .then(() => { ready = true; postMessage({ type: "ready", ms: performance.now() - t0 }); run(); })
  .catch((e) => postMessage({ type: "fatal", message: String(e?.message || e) }));
