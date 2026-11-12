// Example library for the landing page, ported from packages/app/public/launch/launch.js
// (the hero kernel demo is dropped). One shared WebGL renderer blitted into
// per-figure 2D canvases; only visible figures draw. Scene kit follows phyz.dev.
//
// Materials are physically based and lit by an image-based room environment,
// so brass reads as brass and a board as a board. Every material renders both
// sides: kernel meshes are watertight, but a few imported STL bodies (K1)
// carry inconsistently wound triangles, and culling them punches holes.
import * as THREE from "./vendor/three.module.min.js";
import { OrbitControls } from "./vendor/OrbitControls.js";
import { GLTFLoader } from "./vendor/GLTFLoader.js";
import { RoomEnvironment } from "./vendor/RoomEnvironment.js";

// ---------- palette, from the page's CSS tokens ----------
const css = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
const P = {};
function readPalette() {
  const probe = document.createElement("span");
  document.body.appendChild(probe);
  // Paint the token into a 1×1 canvas and read the pixel back: CSS may
  // resolve color-mix() to oklab(), which THREE.Color can't parse.
  const px = document.createElement("canvas").getContext("2d", { willReadFrequently: true });
  const resolve = (v) => {
    probe.style.color = v;
    px.clearRect(0, 0, 1, 1); px.fillStyle = getComputedStyle(probe).color; px.fillRect(0, 0, 1, 1);
    const [r, g, b] = px.getImageData(0, 0, 1, 1).data;
    return new THREE.Color().setRGB(r / 255, g / 255, b / 255, THREE.SRGBColorSpace);
  };
  P.bg = resolve(css("--surface"));
  P.fg = resolve(css("--fg"));
  P.flame = resolve(css("--flame-ink"));
  P.dark = P.bg.getHSL({}).l < 0.4;
  probe.remove();
}

// ---------- shared renderer + environment ----------
const sharedCanvas = document.createElement("canvas");
const renderer = new THREE.WebGLRenderer({ canvas: sharedCanvas, antialias: true, alpha: false, preserveDrawingBuffer: true });
renderer.outputColorSpace = THREE.SRGBColorSpace;
renderer.toneMapping = THREE.ACESFilmicToneMapping;
renderer.toneMappingExposure = 0.78;
const pmrem = new THREE.PMREMGenerator(renderer);
const envMap = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;

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
  scene.environment = envMap;
  const key = new THREE.DirectionalLight(0xffffff, 0.8);
  key.position.set(1, 2, 1.5);
  scene.add(key);
  const camera = new THREE.PerspectiveCamera(28, 4 / 3, 0.1, 10000);
  const controls = new OrbitControls(camera, canvas);
  controls.enableDamping = true;
  controls.dampingFactor = 0.1;
  controls.enablePan = false;
  controls.enableZoom = false;
  controls.autoRotate = autoRotate;
  controls.autoRotateSpeed = 0.8;
  controls.addEventListener("start", () => { controls.autoRotate = false; });
  const root = new THREE.Group();
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



// T-junction repair. Where the kernel's tessellation leaves a vertex sitting
// on another triangle's edge, the two sides don't share that edge and a
// hairline crack opens (the interior shows through). Find edges used by one
// triangle, find boundary vertices lying on them, and split those triangles
// into fans through the stray vertices so the surface closes.
function stitchTJunctions(src) {
  const sp = src.attributes.position, sn = src.attributes.normal;
  const n = sp.count;
  const pos = new Float32Array(n * 3), nrm = sn ? new Float32Array(n * 3) : null;
  for (let i = 0; i < n; i++) {
    pos[i * 3] = sp.getX(i); pos[i * 3 + 1] = sp.getY(i); pos[i * 3 + 2] = sp.getZ(i);
    if (nrm) { nrm[i * 3] = sn.getX(i); nrm[i * 3 + 1] = sn.getY(i); nrm[i * 3 + 2] = sn.getZ(i); }
  }
  const idx = src.index ? Array.from(src.index.array) : Array.from({ length: n }, (_, i) => i);
  src.computeBoundingBox();
  const size = src.boundingBox.getSize(new THREE.Vector3()).length() || 1;
  const q = 1e5 / size, eps = size * 2e-5;
  const key = new Int32Array(n), ids = new Map();
  for (let i = 0; i < n; i++) {
    const k = Math.round(pos[i * 3] * q) + "," + Math.round(pos[i * 3 + 1] * q) + "," + Math.round(pos[i * 3 + 2] * q);
    let id = ids.get(k); if (id === undefined) { id = ids.size; ids.set(k, id); } key[i] = id;
  }
  const count = new Map();
  const ek = (a, b) => (key[a] < key[b] ? key[a] * 4194304 + key[b] : key[b] * 4194304 + key[a]);
  for (let t = 0; t < idx.length; t += 3)
    for (let k = 0; k < 3; k++) { const e = ek(idx[t + k], idx[t + (k + 1) % 3]); count.set(e, (count.get(e) || 0) + 1); }
  const boundaryVerts = new Map();          // weld id -> a representative index
  const open = [];
  for (let t = 0; t < idx.length; t += 3)
    for (let k = 0; k < 3; k++) {
      const a = idx[t + k], b = idx[t + (k + 1) % 3];
      if (key[a] === key[b] || count.get(ek(a, b)) !== 1) continue;
      open.push([t, k]); boundaryVerts.set(key[a], a); boundaryVerts.set(key[b], b);
    }
  if (!open.length) return null;
  const bv = [...boundaryVerts.values()];
  const A = new THREE.Vector3(), B = new THREE.Vector3(), V = new THREE.Vector3(), AB = new THREE.Vector3();
  const replace = new Map();                // triangle start -> replacement triangles
  for (const [t, k] of open) {
    if (replace.has(t)) continue;
    const a = idx[t + k], b = idx[t + (k + 1) % 3], c = idx[t + (k + 2) % 3];
    A.fromArray(pos, a * 3); B.fromArray(pos, b * 3); AB.subVectors(B, A);
    const len2 = AB.lengthSq(); if (len2 < eps * eps) continue;
    const on = [];
    for (const v of bv) {
      if (key[v] === key[a] || key[v] === key[b]) continue;
      V.fromArray(pos, v * 3);
      const s = V.clone().sub(A).dot(AB) / len2;
      if (s <= 1e-4 || s >= 1 - 1e-4) continue;
      if (V.clone().sub(A).addScaledVector(AB, -s).lengthSq() > eps * eps) continue;
      on.push([s, v]);
    }
    if (!on.length) continue;
    on.sort((x, y) => x[0] - y[0]);
    const chain = [a, ...on.map((o) => o[1]), b], tris = [];
    for (let i = 0; i < chain.length - 1; i++) tris.push(chain[i], chain[i + 1], c);
    replace.set(t, tris);
  }
  if (!replace.size) return null;
  const out = [];
  for (let t = 0; t < idx.length; t += 3) {
    const r = replace.get(t);
    if (r) out.push(...r); else out.push(idx[t], idx[t + 1], idx[t + 2]);
  }
  const g = new THREE.BufferGeometry();
  g.setAttribute("position", new THREE.BufferAttribute(pos, 3));
  if (nrm) g.setAttribute("normal", new THREE.BufferAttribute(nrm, 3));
  g.setIndex(out);
  return g;
}

// Auto-smooth. Exported meshes share vertices (and average normals) across
// sharp edges, so long thin triangles on a flat face pick up the curvature of
// a neighbouring bore and shade in streaks. Unweld, then give any corner whose
// normal strays more than `angle` from its triangle the triangle's own normal.
// The triangle normal is oriented by the vertex normals, not by winding, so a
// mis-wound triangle shades the same as its neighbours.
function autoSmooth(src, angle = 35) {
  const g = src.index ? src.toNonIndexed() : src.clone();
  const pos = g.attributes.position;
  const had = !!g.attributes.normal;
  const nrm = had ? g.attributes.normal : new THREE.BufferAttribute(new Float32Array(pos.count * 3), 3);
  const out = new Float32Array(pos.count * 3);
  const cos = Math.cos(THREE.MathUtils.degToRad(angle));
  const a = new THREE.Vector3(), b = new THREE.Vector3(), c = new THREE.Vector3();
  const fn = new THREE.Vector3(), vn = new THREE.Vector3(), avg = new THREE.Vector3();
  for (let i = 0; i < pos.count; i += 3) {
    a.fromBufferAttribute(pos, i); b.fromBufferAttribute(pos, i + 1); c.fromBufferAttribute(pos, i + 2);
    fn.subVectors(c, b).cross(a.clone().sub(b));
    // A sliver's face normal is rounding noise; trust its vertex normals.
    const sliver = fn.lengthSq() < 1e-12 * Math.max(a.distanceToSquared(b), b.distanceToSquared(c), 1e-30) ** 2 || !isFinite(fn.x);
    fn.normalize();
    if (had && sliver) {
      for (let k = 0; k < 3; k++) { vn.fromBufferAttribute(nrm, i + k).normalize(); out.set([vn.x, vn.y, vn.z], (i + k) * 3); }
      continue;
    }
    if (had) {
      avg.set(0, 0, 0);
      for (let k = 0; k < 3; k++) avg.add(vn.fromBufferAttribute(nrm, i + k));
      if (fn.dot(avg) < 0) {
        // Mis-wound: swap two corners in every attribute so the winding agrees
        // with the normal. Otherwise a double-sided material treats the
        // triangle as back-facing and flips its shading.
        for (const attr of Object.values(g.attributes)) {
          const w = attr.itemSize, arr = attr.array;
          for (let e = 0; e < w; e++) { const t = arr[(i + 1) * w + e]; arr[(i + 1) * w + e] = arr[(i + 2) * w + e]; arr[(i + 2) * w + e] = t; }
        }
        a.fromBufferAttribute(pos, i); b.fromBufferAttribute(pos, i + 1); c.fromBufferAttribute(pos, i + 2);
        fn.subVectors(c, b).cross(a.clone().sub(b)).normalize();
      }
    }
    for (let k = 0; k < 3; k++) {
      if (had) vn.fromBufferAttribute(nrm, i + k).normalize();
      const use = had && vn.dot(fn) >= cos ? vn : fn;
      out[(i + k) * 3] = use.x; out[(i + k) * 3 + 1] = use.y; out[(i + k) * 3 + 2] = use.z;
    }
  }
  g.setAttribute("normal", new THREE.BufferAttribute(out, 3));
  return g;
}

// Feature edges as hairlines. Faint, so the shading carries the form and the
// lines only sharpen silhouettes and creases.
// three's EdgesGeometry compares winding-based normals, so one mis-wound
// triangle in a flat face reads as a 180° fold and draws a stray line. This
// version welds by position and compares normals by |cos|: a crease is a real
// change of direction, not a flipped triangle.
function creaseGeometry(geometry, angle) {
  const pos = geometry.attributes.position;
  const idx = geometry.index ? geometry.index.array : null;
  const tri = idx ? idx.length / 3 : pos.count / 3;
  const vi = (t, k) => (idx ? idx[t * 3 + k] : t * 3 + k);
  // weld: quantize positions to a grid relative to the mesh size
  geometry.computeBoundingBox();
  const size = geometry.boundingBox.getSize(new THREE.Vector3()).length() || 1;
  const q = 1e5 / size;
  const keyOf = new Int32Array(pos.count);
  const ids = new Map();
  for (let i = 0; i < pos.count; i++) {
    const k = Math.round(pos.getX(i) * q) + "," + Math.round(pos.getY(i) * q) + "," + Math.round(pos.getZ(i) * q);
    let id = ids.get(k);
    if (id === undefined) { id = ids.size; ids.set(k, id); }
    keyOf[i] = id;
  }
  const a = new THREE.Vector3(), b = new THREE.Vector3(), c = new THREE.Vector3();
  const normals = new Float32Array(tri * 3);
  for (let t = 0; t < tri; t++) {
    a.fromBufferAttribute(pos, vi(t, 0)); b.fromBufferAttribute(pos, vi(t, 1)); c.fromBufferAttribute(pos, vi(t, 2));
    const n = c.clone().sub(b).cross(a.clone().sub(b)).normalize();
    normals.set([n.x, n.y, n.z], t * 3);
  }
  const edges = new Map();
  const cos = Math.cos(THREE.MathUtils.degToRad(angle));
  const out = [];
  for (let t = 0; t < tri; t++) {
    for (let k = 0; k < 3; k++) {
      const i0 = vi(t, k), i1 = vi(t, (k + 1) % 3);
      const k0 = keyOf[i0], k1 = keyOf[i1];
      if (k0 === k1) continue;
      const key = k0 < k1 ? k0 * 4194304 + k1 : k1 * 4194304 + k0;
      const prev = edges.get(key);
      if (prev === undefined) { edges.set(key, { t, i0, i1, n: 1 }); continue; }
      prev.n++;
      const d = Math.abs(normals[t * 3] * normals[prev.t * 3] + normals[t * 3 + 1] * normals[prev.t * 3 + 1] + normals[t * 3 + 2] * normals[prev.t * 3 + 2]);
      if (d < cos) prev.crease = true;
    }
  }
  for (const e of edges.values()) {
    // Creases only. In a closed solid an edge with one triangle is a
    // tessellation T-junction, not a real boundary, and drawing it puts a
    // stray line across a flat face.
    if (!e.crease) continue;
    out.push(pos.getX(e.i0), pos.getY(e.i0), pos.getZ(e.i0), pos.getX(e.i1), pos.getY(e.i1), pos.getZ(e.i1));
  }
  const g = new THREE.BufferGeometry();
  g.setAttribute("position", new THREE.BufferAttribute(new Float32Array(out), 3));
  return g;
}
function edgeLines(geometry, angle = 35) {
  const m = new THREE.LineBasicMaterial({ color: P.fg, transparent: true, opacity: P.dark ? 0.22 : 0.28, depthWrite: false });
  const l = new THREE.LineSegments(creaseGeometry(geometry, angle), m);
  l.userData.edge = true;
  return l;
}
function restyle(f) {
  f.root.traverse((o) => {
    if (o.isLineSegments && o.userData.edge) { o.material.color.copy(P.fg); o.material.opacity = P.dark ? 0.22 : 0.28; }
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

// ---------- loading states: every example says what it's doing ----------
// An overlay over each canvas: a progress bar while the model downloads,
// "preparing" while it's processed, and a retry button if it fails or stalls.
const STALL_MS = 25000;
function loadStatus(canvas, retry) {
  const stage = document.createElement("div");
  stage.className = "ex-stage";
  canvas.replaceWith(stage);
  stage.appendChild(canvas);
  const el = document.createElement("div");
  el.className = "ex-status";
  el.setAttribute("role", "status");
  stage.appendChild(el);
  const ex = canvas.closest(".ex");
  let stall = 0;
  const api = {
    loading(frac) {
      ex?.classList.remove("failed");
      clearTimeout(stall);
      stall = setTimeout(() => api.failed("still loading — slow connection?"), STALL_MS);
      const pct = frac == null ? null : Math.round(frac * 100);
      el.innerHTML = `<span class="ex-status-label">${pct == null ? "loading model" : `loading model · ${pct}%`}</span>` +
        `<span class="ex-status-bar${pct == null ? " indeterminate" : ""}"><i style="width:${pct ?? 30}%"></i></span>`;
      el.hidden = false;
    },
    preparing() {
      clearTimeout(stall);
      el.innerHTML = `<span class="ex-status-label">preparing</span><span class="ex-status-bar indeterminate"><i></i></span>`;
    },
    ready() { clearTimeout(stall); el.hidden = true; el.innerHTML = ""; ex?.classList.add("ready"); },
    failed(msg = "couldn't load this example") {
      clearTimeout(stall);
      if (ex?.classList.contains("ready")) return;
      ex?.classList.add("failed");
      el.hidden = false;
      el.innerHTML = `<span class="ex-status-label">${msg}</span><button type="button" class="ex-retry">retry</button>`;
      el.querySelector("button").onclick = () => retry();
    },
  };
  el.hidden = true;
  return api;
}
// let the overlay paint before a heavy synchronous step
// (rAF never fires in a background tab, so fall back to a timer)
const nextPaint = () => new Promise((r) => { requestAnimationFrame(() => setTimeout(r, 0)); setTimeout(r, 50); });

// ---------- gallery: every sample, orbitable ----------
// Models come from `vcad-render --export-web`: one named node per part, each
// with the document's own PBR material, Y-up and world-placed already.
const loader = new GLTFLoader();
function galleryFigure(canvas) {
  const f = figure(canvas);
  let loaded = false;
  const st = loadStatus(canvas, () => { loaded = false; f.wake(); });
  f.wake = () => {
    if (loaded) return;
    loaded = true;
    let tries = 0;
    st.loading(null);
    const fetchModel = () => loader.load(canvas.dataset.model, async (gltf) => {
      st.preparing();
      await nextPaint();
      try {
        gltf.scene.traverse((o) => {
          if (!o.isMesh) return;
          const mats = Array.isArray(o.material) ? o.material : [o.material];
          for (const m of mats) {
            m.side = THREE.DoubleSide; m.envMapIntensity = 0.75; m.flatShading = false;
            // vcad-render's fallback material ("part"/"default") is a flat grey
            // dielectric that reads as plaster; show it as bead-blasted aluminum.
            if (m.metalness < 0.05 && m.color.r > 0.55 && Math.abs(m.color.r - m.color.b) < 0.08 && /^(part|default)$/.test(m.name)) {
              m.color.setRGB(0.78, 0.79, 0.81); m.metalness = 0.8; m.roughness = 0.42;
            }
            m.needsUpdate = true;
          }
          const original = stitchTJunctions(o.geometry) || o.geometry;
          o.geometry = autoSmooth(original);
          o.add(edgeLines(original));
        });
        if (canvas.dataset.board) dressBoard(gltf.scene, canvas.dataset.board);
        f.root.add(gltf.scene);
        f.root.updateMatrixWorld(true);
        frame(f, new THREE.Box3().setFromObject(gltf.scene), (canvas.dataset.view || "1,0.75,1.1").split(",").map(Number));
        st.ready();
      } catch (e) {
        console.warn("example failed to prepare:", canvas.dataset.model, e);
        st.failed();
      }
    }, (ev) => {
      if (ev.lengthComputable && ev.total) st.loading(ev.loaded / ev.total);
    }, () => {
      if (tries++ < 2) setTimeout(fetchModel, 800 * tries);
      else st.failed();
    });
    fetchModel();
  };
}


// ---------- boards: photoreal, from the board's own data ----------
// scripts/launch/pcb-board-assets.py draws the soldermask, copper, ENIG pads,
// drills and silkscreen into textures, and lists every footprint's body box.
// Here the board is extruded from its outline (cutouts included) and each
// footprint becomes a package of the right kind.
const texLoader = new THREE.TextureLoader();
function boardTexture(url, srgb) {
  const t = texLoader.load(url);
  t.colorSpace = srgb ? THREE.SRGBColorSpace : THREE.NoColorSpace;
  t.anisotropy = 8;
  return t;
}
const PKG = {
  body: new THREE.MeshStandardMaterial({ color: 0x141414, roughness: 0.55, metalness: 0.0 }),
  tin: new THREE.MeshStandardMaterial({ color: 0xd6d6d2, roughness: 0.3, metalness: 1.0 }),
  gold: new THREE.MeshStandardMaterial({ color: 0xd9b45a, roughness: 0.25, metalness: 1.0 }),
  ceramic: new THREE.MeshStandardMaterial({ color: 0xa88b5e, roughness: 0.6, metalness: 0.0 }),
  steel: new THREE.MeshStandardMaterial({ color: 0xc7c9cc, roughness: 0.32, metalness: 1.0 }),
  led: new THREE.MeshPhysicalMaterial({ color: 0xf4f7f2, roughness: 0.15, metalness: 0.0, transmission: 0.4, thickness: 0.5 }),
  plastic: new THREE.MeshStandardMaterial({ color: 0x2a2a2a, roughness: 0.7, metalness: 0.0 }),
  grey: new THREE.MeshStandardMaterial({ color: 0x8c8f93, roughness: 0.5, metalness: 0.2 }),
  fr4: new THREE.MeshStandardMaterial({ color: 0xcfc39a, roughness: 0.8, metalness: 0.0 }),
};
function box(w, d, h, mat, x = 0, y = 0, z = 0) {
  const m = new THREE.Mesh(new THREE.BoxGeometry(w, d, h), mat);
  m.position.set(x, y, z + h / 2);
  return m;
}
function component(c) {
  const g = new THREE.Group();
  const [x0, y0, x1, y1] = c.box;
  const w = Math.max(x1 - x0, 0.3), d = Math.max(y1 - y0, 0.3), cx = (x0 + x1) / 2, cy = (y0 + y1) / 2, h = c.height;
  const along = w >= d;                 // terminations sit on the long axis
  switch (c.class) {
    case "resistor":
    case "capacitor": {
      const body = c.class === "resistor" ? PKG.body : PKG.ceramic;
      const L = along ? w * 0.9 : d * 0.9, W = along ? d * 0.75 : w * 0.75, cap = L * 0.22;
      g.add(box(along ? L - 2 * cap : W, along ? W : L - 2 * cap, h, body, cx, cy));
      for (const s of [-1, 1]) {
        const off = s * (L / 2 - cap / 2);
        g.add(box(along ? cap : W, along ? W : cap, h, PKG.tin, cx + (along ? off : 0), cy + (along ? 0 : off)));
      }
      break;
    }
    case "led":
      g.add(box(w * 0.85, d * 0.8, h, PKG.led, cx, cy));
      break;
    case "can":
      g.add(box(w * 0.92, d * 0.92, h, PKG.steel, cx, cy));
      break;
    case "usb":
      g.add(box(w * 0.95, d * 0.95, h, PKG.steel, cx, cy));
      break;
    case "switch":
      g.add(box(w * 0.9, d * 0.6, h, PKG.grey, cx, cy));
      g.add(new THREE.Mesh(new THREE.CylinderGeometry(Math.min(w, d) * 0.18, Math.min(w, d) * 0.18, 0.6, 24), PKG.plastic)
        .rotateX(Math.PI / 2).translateY(h + 0.3).translateX(cx).translateZ(-cy));
      break;
    case "header": {
      g.add(box(w * 0.98, d * 0.98, h, PKG.plastic, cx, cy));
      const pin = new THREE.BoxGeometry(0.64, 0.64, 8.5);
      for (const [px, py] of c.pins) {
        const m = new THREE.Mesh(pin, PKG.gold);
        m.position.set(px, py, 8.5 / 2 - 3);
        g.add(m);
      }
      break;
    }
    default: {                          // ICs: a moulded body over its leads
      const inset = c.class === "ic-flat" ? 0.98 : 0.8;
      g.add(box(w * inset, d * (c.class === "ic-flat" ? 0.98 : 0.92), h, PKG.body, cx, cy, 0.1));
    }
  }
  return g;
}

// A board inside an assembly (the PCB motor's stator) exports as one green
// solid. Give its top and bottom faces the board's photoreal textures,
// mapped through the mesh's own extents (glTF is Y-up: board x → x,
// board y → −z).
function dressBoard(scene, base) {
  scene.traverse((o) => {
    // the board is the soldermask-green body (material names aren't unique)
    const col = o.material?.color;
    if (!o.isMesh || !col || !(col.g > col.r * 2.5 && col.g > col.b * 1.4)) return;
    const g = o.geometry, pos = g.attributes.position, nrm = g.attributes.normal;
    g.computeBoundingBox();
    const { min, max } = g.boundingBox, w = max.x - min.x, d = max.z - min.z;
    const uv = new Float32Array(pos.count * 2);
    const top = [], bottom = [], sides = [];
    for (let i = 0; i < pos.count; i++) {
      const u = (pos.getX(i) - min.x) / w, v = (max.z - pos.getZ(i)) / d;
      uv.set([u, v], i * 2);
    }
    g.setAttribute("uv", new THREE.BufferAttribute(uv, 2));
    // split triangles by facing into three groups
    const idx = g.index ? g.index.array : null, tri = idx ? idx.length / 3 : pos.count / 3;
    const order = [];
    for (let t = 0; t < tri; t++) {
      let ny = 0;
      for (let k = 0; k < 3; k++) ny += nrm.getY(idx ? idx[t * 3 + k] : t * 3 + k);
      (ny > 2.7 ? top : ny < -2.7 ? bottom : sides).push(t);
    }
    const newIdx = [];
    g.clearGroups();
    for (const [list, m] of [[top, 0], [bottom, 1], [sides, 2]]) {
      const start = newIdx.length;
      for (const t of list) for (let k = 0; k < 3; k++) newIdx.push(idx ? idx[t * 3 + k] : t * 3 + k);
      g.addGroup(start, newIdx.length - start, m);
    }
    g.setIndex(newIdx);
    // Colour only: on this mesh the roughness/metal map renders black, and
    // the ring is almost all soldermask, so constant mask values read right.
    const mat = (side, flipU) => {
      const map = boardTexture(`${base}-${side}.jpg`, true);
      if (flipU) { map.wrapS = THREE.RepeatWrapping; map.repeat.x = -1; }
      return new THREE.MeshStandardMaterial({ map, roughness: 0.45, metalness: 0.0, side: THREE.DoubleSide });
    };
    o.material = [mat("top", false), mat("bottom", true), PKG.fr4];
    o.userData.board = true;
  });
}

function pcbFigure(canvas) {
  const f = figure(canvas);
  f.root.rotation.x = -Math.PI / 2;            // board data is Z-up
  let loaded = false;
  const st = loadStatus(canvas, () => { loaded = false; f.root.clear(); f.wake(); });
  f.wake = async () => {
    if (loaded) return;
    loaded = true;
    st.loading(null);
    try { await buildBoard(); } catch (e) {
      console.warn("board example failed:", canvas.dataset.pcb, e);
      st.failed();
    }
  };
  async function buildBoard() {
    const url = canvas.dataset.pcb;
    const base = url.replace(/\.pcb\.json$/, "");
    const res = await fetch(url);
    if (!res.ok) throw new Error(`${url}: ${res.status}`);
    const b = await res.json();
    st.preparing();
    await nextPaint();
    const [bx0, by0, bx1, by1] = b.bbox, bw = bx1 - bx0, bh = by1 - by0, t = b.thickness;
    const shape = new THREE.Shape(b.outline.map(([x, y]) => new THREE.Vector2(x, y)));
    for (const c of b.cutouts) shape.holes.push(new THREE.Path(c.map(([x, y]) => new THREE.Vector2(x, y))));
    const side = (name, z, flip) => {
      const geo = new THREE.ShapeGeometry(shape, 48);
      const uv = geo.attributes.uv, pos = geo.attributes.position;
      for (let i = 0; i < pos.count; i++) {
        const u = (pos.getX(i) - bx0) / bw;
        uv.setXY(i, flip ? 1 - u : u, (pos.getY(i) - by0) / bh);
      }
      if (flip) {                                  // face down
        const idx = geo.index.array;
        for (let i = 0; i < idx.length; i += 3) { const k = idx[i + 1]; idx[i + 1] = idx[i + 2]; idx[i + 2] = k; }
        geo.computeVertexNormals();
      }
      geo.translate(0, 0, z);
      const rm = boardTexture(`${base}-${name}-rm.jpg`, false);
      return new THREE.Mesh(geo, new THREE.MeshStandardMaterial({
        map: boardTexture(`${base}-${name}.jpg`, true), roughnessMap: rm, metalnessMap: rm, roughness: 1, metalness: 1,
      }));
    };
    f.root.add(side("top", t, false), side("bottom", 0, true));
    const edge = new THREE.ExtrudeGeometry(shape, { depth: t, bevelEnabled: false, curveSegments: 48 });
    f.root.add(new THREE.Mesh(edge, [new THREE.MeshBasicMaterial({ visible: false }), PKG.fr4]));
    for (const c of b.components) {
      const g = component(c);
      g.rotation.z = THREE.MathUtils.degToRad(c.rotation);
      if (!c.front) { g.scale.x = -1; g.rotation.x = Math.PI; }
      g.position.set(c.x, c.y, c.front ? t : 0);
      f.root.add(g);
    }
    f.root.updateMatrixWorld(true);
    frame(f, new THREE.Box3().setFromObject(f.root), (canvas.dataset.view || "0.55,1.15,0.9").split(",").map(Number));
    st.ready();
  }
}

// ---------- boot ----------
readPalette();
document.querySelectorAll("canvas[data-model]").forEach(galleryFigure);
document.querySelectorAll("canvas[data-pcb]").forEach(pcbFigure);
const onTheme = () => { readPalette(); figures.forEach(restyle); };
new MutationObserver(onTheme).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
matchMedia("(prefers-color-scheme: dark)").addEventListener("change", onTheme);
requestAnimationFrame(loop);
