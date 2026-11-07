// Live figures for the launch page: the hero bracket, rebuilt by the real
// kernel (hero-worker.js) as you drag, and every sample in the gallery as an
// orbitable model. The "Pilot Light" scene kit from phyz.dev: one shared WebGL
// renderer blitted into per-figure 2D canvases, only visible figures drawn,
// flat warm fills, hairline edges, one flame per figure.
import * as THREE from "./vendor/three.module.min.js";
import { OrbitControls } from "./vendor/OrbitControls.js";
import { GLTFLoader } from "./vendor/GLTFLoader.js";

// ---------- palette, from the page's CSS tokens ----------
const css = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
const P = {};
function readPalette() {
  const probe = document.createElement("span");
  document.body.appendChild(probe);
  const resolve = (v) => { probe.style.color = v; return new THREE.Color(getComputedStyle(probe).color); };
  P.bg = resolve(css("--surface"));
  P.fg = resolve(css("--fg"));
  P.muted = resolve(css("--muted"));
  P.flame = resolve(css("--flame-ink"));
  P.dark = P.bg.getHSL({}).l < 0.4;
  P.fill = P.dark ? new THREE.Color("#5c4c3a") : new THREE.Color("#e9e1d2");
  probe.remove();
}

// ---------- shared renderer ----------
const sharedCanvas = document.createElement("canvas");
const renderer = new THREE.WebGLRenderer({ canvas: sharedCanvas, antialias: true, alpha: false, preserveDrawingBuffer: true });
renderer.outputColorSpace = THREE.SRGBColorSpace;
const figures = new Map();   // canvas -> figure
const visible = new Set();
const io = new IntersectionObserver((entries) => {
  for (const e of entries) {
    const f = figures.get(e.target);
    if (!f) continue;
    if (e.isIntersecting) { visible.add(f); f.wake?.(); } else visible.delete(f);
  }
}, { rootMargin: "200px" });

function figure(canvas, { autoRotate = true } = {}) {
  const scene = new THREE.Scene();
  const camera = new THREE.PerspectiveCamera(28, 4 / 3, 0.1, 10000);
  const controls = new OrbitControls(camera, canvas);
  controls.enableDamping = true;
  controls.dampingFactor = 0.1;
  controls.enablePan = false;
  controls.enableZoom = false;
  controls.autoRotate = autoRotate;
  controls.autoRotateSpeed = 0.8;
  controls.addEventListener("start", () => { controls.autoRotate = false; });
  scene.add(new THREE.HemisphereLight(0xffffff, 0x8a7658, 2.2));
  const key = new THREE.DirectionalLight(0xffffff, 1.1);
  key.position.set(1, 2, 1.5);
  scene.add(key);
  // Kernel geometry is Z-up; three is Y-up.
  const root = new THREE.Group();
  root.rotation.x = -Math.PI / 2;
  scene.add(root);
  const f = { canvas, scene, camera, controls, root, ctx: canvas.getContext("2d") };
  figures.set(canvas, f);
  io.observe(canvas);
  return f;
}

function frame(f, box, dir = [1, 0.75, 1.1]) {
  const size = box.getSize(new THREE.Vector3());
  const center = box.getCenter(new THREE.Vector3());
  const radius = size.length() / 2 || 1;
  const dist = radius / Math.sin(THREE.MathUtils.degToRad(f.camera.fov / 2)) * 0.92;
  const d = new THREE.Vector3(...dir).normalize().multiplyScalar(dist);
  f.camera.position.copy(center).add(d);
  f.camera.near = dist / 100; f.camera.far = dist * 10;
  f.camera.updateProjectionMatrix();
  f.controls.target.copy(center);
  f.controls.update();
}

// Pilot Light part style: warm flat fill, hairline feature edges.
function fillMaterial(vertexColors = false) {
  return new THREE.MeshLambertMaterial({ color: vertexColors ? 0xffffff : P.fill, vertexColors, polygonOffset: true, polygonOffsetFactor: 1, polygonOffsetUnits: 1 });
}
function edgeLines(geometry, angle = 28) {
  const m = new THREE.LineBasicMaterial({ color: P.fg, transparent: true, opacity: P.dark ? 0.55 : 0.7 });
  const l = new THREE.LineSegments(new THREE.EdgesGeometry(geometry, angle), m);
  l.userData.edge = true;
  return l;
}
function tint(mesh) {
  const c = mesh.userData.tint;
  mesh.material.color.copy(P.fill);
  if (c) mesh.material.color.lerp(c, P.dark ? 0.35 : 0.45);
}
function restyle(f) {
  f.root.traverse((o) => {
    if (o.isLineSegments && o.userData.edge) { o.material.color.copy(P.fg); o.material.opacity = P.dark ? 0.55 : 0.7; }
    else if (o.isMesh && !o.material.vertexColors) tint(o);
  });
  f.recolor?.();
}

function loop() {
  requestAnimationFrame(loop);
  for (const f of visible) {
    const rect = f.canvas.getBoundingClientRect();
    if (!rect.width || !rect.height) continue;
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const pw = Math.floor(rect.width * dpr), ph = Math.floor(rect.height * dpr);
    if (f.canvas.width !== pw || f.canvas.height !== ph) { f.canvas.width = pw; f.canvas.height = ph; }
    renderer.setSize(pw, ph, false);
    renderer.setClearColor(P.bg);
    f.camera.aspect = rect.width / rect.height;
    f.camera.updateProjectionMatrix();
    f.controls.update();
    renderer.render(f.scene, f.camera);
    f.ctx.drawImage(sharedCanvas, 0, 0);
  }
}

// ---------- gallery: every sample, orbitable ----------
const loader = new GLTFLoader();
function galleryFigure(canvas) {
  const f = figure(canvas);
  let loaded = false;
  f.wake = () => {
    if (loaded) return;
    loaded = true;
    let tries = 0;
    const fetchModel = () => loader.load(canvas.dataset.model, (gltf) => {
      const meshes = [];
      gltf.scene.traverse((o) => { if (o.isMesh) meshes.push(o); });
      gltf.scene.updateMatrixWorld(true);
      for (const m of meshes) {
        // gltfpack stores positions as normalized int16 (KHR_mesh_quantization);
        // dequantize to float before baking the node transform in.
        const src = m.geometry.attributes.position;
        const pos = new Float32Array(src.count * 3);
        for (let i = 0; i < src.count; i++) { pos[i * 3] = src.getX(i); pos[i * 3 + 1] = src.getY(i); pos[i * 3 + 2] = src.getZ(i); }
        const g = new THREE.BufferGeometry();
        g.setAttribute("position", new THREE.BufferAttribute(pos, 3));
        if (m.geometry.index) g.setIndex(m.geometry.index.clone());
        g.applyMatrix4(m.matrixWorld);
        g.computeVertexNormals();
        // Keep a hint of each part's own material (board green, brass, steel)
        // mixed into the warm fill, so a board still reads as a board.
        const mesh = new THREE.Mesh(g, fillMaterial());
        mesh.userData.tint = m.material?.color?.clone?.() ?? null;
        tint(mesh);
        f.root.add(mesh);
        f.root.add(edgeLines(g));
      }
      f.root.updateMatrixWorld(true);
      frame(f, new THREE.Box3().setFromObject(f.root));
      canvas.closest(".ex")?.classList.add("ready");
    }, undefined, () => {
      if (tries++ < 2) setTimeout(fetchModel, 800 * tries);
      else canvas.closest(".ex")?.classList.add("failed");
    });
    fetchModel();
  };
}

// ---------- hero: the kernel, live ----------
function hero() {
  const canvas = document.getElementById("hero-canvas");
  if (!canvas) return;
  const f = figure(canvas);
  const status = document.getElementById("hero-status");
  const say = (t) => { if (status) status.textContent = t; };
  const ro = (id, v) => { const el = document.getElementById(id); if (el) el.textContent = v; };
  const fmt = (x, d = 0) => x.toLocaleString("en-US", { minimumFractionDigits: d, maximumFractionDigits: d });

  const inputs = [...document.querySelectorAll("[data-param]")];
  const params = () => Object.fromEntries(inputs.map((i) => [i.dataset.param, parseFloat(i.value)]));
  const showValues = () => inputs.forEach((i) => { const o = document.getElementById("v-" + i.dataset.param); if (o) o.textContent = i.value + (i.dataset.unit || ""); });

  let part = null, edges = null, kinds = null, framed = false, seq = 0, sent = 0;
  const colorize = (geo) => {
    // One flame per figure: every cylindrical face (the drilled holes and
    // the bore) in coral, straight from the kernel's per-triangle face kinds.
    const pos = geo.attributes.position;
    const col = new Float32Array(pos.count * 3);
    for (let t = 0; t < pos.count / 3; t++) {
      const c = kinds && kinds[t] === 2 ? P.flame : P.fill;
      for (let k = 0; k < 3; k++) col.set([c.r, c.g, c.b], (t * 3 + k) * 3);
    }
    geo.setAttribute("color", new THREE.BufferAttribute(col, 3));
  };
  f.recolor = () => { if (part) colorize(part.geometry); };

  const worker = new Worker(new URL("./hero-worker.js", import.meta.url), { type: "module" });
  const request = () => { seq++; sent = seq; worker.postMessage({ type: "build", id: seq, params: params() }); say("solving…"); };
  worker.onmessage = (e) => {
    const m = e.data;
    if (m.type === "ready") { say(`kernel loaded in ${fmt(m.ms)} ms`); document.body.classList.add("kernel-ready"); request(); }
    if (m.type === "fatal") { say("couldn't start the kernel in this browser"); document.body.classList.add("kernel-failed"); }
    if (m.type === "error") say("that combination didn't solve, try another");
    if (m.type === "step") {
      const url = URL.createObjectURL(new Blob([m.bytes], { type: "model/step" }));
      const a = Object.assign(document.createElement("a"), { href: url, download: "vcad-bracket.step" });
      document.body.appendChild(a); a.click(); a.remove();
      setTimeout(() => URL.revokeObjectURL(url), 2000);
    }
    if (m.type !== "mesh") return;
    kinds = m.kinds;
    const indexed = new THREE.BufferGeometry();
    indexed.setAttribute("position", new THREE.BufferAttribute(m.positions, 3));
    indexed.setIndex(new THREE.BufferAttribute(m.indices, 1));
    const geo = indexed.toNonIndexed();
    geo.computeVertexNormals();
    colorize(geo);
    if (part) { f.root.remove(part, edges); part.geometry.dispose(); edges.geometry.dispose(); }
    part = new THREE.Mesh(geo, fillMaterial(true));
    edges = edgeLines(indexed);
    f.root.add(part, edges);
    f.root.updateMatrixWorld(true);
    if (!framed) { frame(f, new THREE.Box3().setFromObject(f.root), [1.1, 0.7, 1.25]); framed = true; }
    ro("r-vol", fmt(m.volume / 1000, 1) + " cm³");
    ro("r-mass", fmt(m.volume * 2.70e-3, 0) + " g");
    ro("r-area", fmt(m.area / 100, 1) + " cm²");
    ro("r-tris", fmt(m.indices.length / 3));
    ro("r-ms", fmt(m.solveMs + m.meshMs) + " ms");
    if (m.id === sent) say(`solved in ${fmt(m.solveMs + m.meshMs)} ms, in this tab`);
  };
  let timer = 0;
  inputs.forEach((i) => i.addEventListener("input", () => { showValues(); clearTimeout(timer); timer = setTimeout(request, 60); }));
  document.getElementById("hero-step")?.addEventListener("click", () => worker.postMessage({ type: "step" }));
  showValues();
  say("loading the kernel (5 MB)…");
}

// ---------- boot ----------
readPalette();
hero();
document.querySelectorAll("canvas[data-model]").forEach(galleryFigure);
new MutationObserver(() => { readPalette(); figures.forEach(restyle); })
  .observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => { readPalette(); figures.forEach(restyle); });
requestAnimationFrame(loop);
