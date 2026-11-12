// Hero: an interactive illustration of vcad.app.
// The agent types a request, its cursor selects the top face, press-pulls a boss,
// and the clearance receipt checks. Then the viewer can drag the boss height
// and watch the receipt flip between Holds and Violated. Pure SVG, no kernel.

const svg = document.getElementById("heroSvg");
if (svg) init(svg);

function init(svg) {
  const NS = "http://www.w3.org/2000/svg";
  const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const el = (tag, attrs = {}, parent = svg, text) => {
    const n = document.createElementNS(NS, tag);
    for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, v);
    if (text != null) n.textContent = text;
    parent.appendChild(n);
    return n;
  };

  // ---- model (mm) --------------------------------------------------------
  const BASE_H = 16, COVER_Z = 46, MIN_CLEAR = 3, H_MAX = 38, AGENT_H = 26;
  const clearance = (h) => COVER_Z - (BASE_H + h);
  const massG = (h) => (160 * 100 * BASE_H + 40 * 40 * h) * 2.7e-3; // aluminium

  // ---- isometric projection ---------------------------------------------
  const S = 2.35, CX = 600, CY = 410, C30 = Math.cos(Math.PI / 6), S30 = 0.5;
  const P = (x, y, z) => [CX + (x - y) * C30 * S, CY + (x + y) * S30 * S - z * S];
  const pts = (a) => a.map((p) => P(...p).map((v) => v.toFixed(1)).join(",")).join(" ");
  function boxFaces(x, y, z, w, d, h) {
    return {
      top: pts([[x, y, z + h], [x + w, y, z + h], [x + w, y + d, z + h], [x, y + d, z + h]]),
      right: pts([[x + w, y, z], [x + w, y + d, z], [x + w, y + d, z + h], [x + w, y, z + h]]),
      front: pts([[x, y + d, z], [x + w, y + d, z], [x + w, y + d, z + h], [x, y + d, z + h]]),
    };
  }

  // ---- chrome ------------------------------------------------------------
  const defs = el("defs");
  const grad = el("radialGradient", { id: "hvBg", cx: "50%", cy: "45%", r: "75%" }, defs);
  el("stop", { offset: "0", "stop-color": "#36363a" }, grad);
  el("stop", { offset: "1", "stop-color": "#232325" }, grad);
  const clip = el("clipPath", { id: "hvClip" }, defs);
  el("rect", { width: 1200, height: 760, rx: 14 }, clip);

  const root = el("g", { "clip-path": "url(#hvClip)" });
  el("rect", { width: 1200, height: 760, fill: "url(#hvBg)" }, root);

  // ground grid (iso)
  const grid = el("g", { stroke: "#ffffff", "stroke-opacity": 0.06, "stroke-width": 1 }, root);
  for (let i = -200; i <= 200; i += 20) {
    const [a, b] = [P(i, -200, -0.01), P(i, 200, -0.01)];
    const [c, d] = [P(-200, i, -0.01), P(200, i, -0.01)];
    el("line", { x1: a[0], y1: a[1], x2: b[0], y2: b[1] }, grid);
    el("line", { x1: c[0], y1: c[1], x2: d[0], y2: d[1] }, grid);
  }
  { const [a, b] = [P(-200, 0, 0), P(200, 0, 0)]; el("line", { x1: a[0], y1: a[1], x2: b[0], y2: b[1], stroke: "#e5484d", "stroke-opacity": 0.45 }, root); }
  { const [a, b] = [P(0, -200, 0), P(0, 200, 0)]; el("line", { x1: a[0], y1: a[1], x2: b[0], y2: b[1], stroke: "#46a758", "stroke-opacity": 0.45 }, root); }

  // traffic lights + title pill
  [["#ff5f57", 28], ["#febc2e", 48], ["#28c840", 68]].forEach(([c, x]) => el("circle", { cx: x, cy: 28, r: 6, fill: c }, root));
  el("rect", { x: 470, y: 14, width: 260, height: 28, rx: 14, fill: "#3a3a3e", stroke: "#4a4a4f" }, root);
  el("text", { x: 600, y: 33, "text-anchor": "middle", class: "hv-t hv-b" }, root, "bracket-cover");

  // header pill (agent job)
  const pill = el("g", { opacity: 0 }, root);
  const pillBg = el("rect", { x: 500, y: 54, width: 200, height: 26, rx: 13, fill: "#1f1f22", stroke: "#4a4a4f" }, pill);
  const pillTxt = el("text", { x: 600, y: 71, "text-anchor": "middle", class: "hv-t hv-s" }, pill, "");

  // navigator
  el("rect", { x: 16, y: 60, width: 200, height: 170, rx: 12, class: "hv-panel" }, root);
  el("text", { x: 32, y: 86, class: "hv-t hv-b" }, root, "Model");
  el("line", { x1: 32, y1: 98, x2: 200, y2: 98, stroke: "#4a4a4f" }, root);
  el("text", { x: 40, y: 124, class: "hv-t" }, root, "Base");
  const navBoss = el("g", { opacity: 0 }, root);
  el("rect", { x: 28, y: 136, width: 176, height: 26, rx: 6, fill: "#4a4a50" }, navBoss);
  el("text", { x: 40, y: 154, class: "hv-t" }, navBoss, "Boss");
  const navBossDim = el("text", { x: 196, y: 154, "text-anchor": "end", class: "hv-t hv-s hv-dim" }, navBoss, "");
  el("text", { x: 40, y: 186, class: "hv-t" }, root, "Cover");
  el("text", { x: 32, y: 216, class: "hv-t hv-s hv-dim" }, root, "receipt · 1 claim");

  // inspector / receipt
  el("rect", { x: 984, y: 60, width: 200, height: 196, rx: 12, class: "hv-panel" }, root);
  el("text", { x: 1000, y: 86, class: "hv-t hv-b" }, root, "Receipt");
  el("line", { x1: 1000, y1: 98, x2: 1168, y2: 98, stroke: "#4a4a4f" }, root);
  el("text", { x: 1000, y: 122, class: "hv-t hv-s hv-dim" }, root, "clearance ≥ 3 mm");
  const recVal = el("text", { x: 1000, y: 146, class: "hv-t hv-big" }, root, "—");
  const recState = el("text", { x: 1168, y: 146, "text-anchor": "end", class: "hv-t hv-b hv-dim" }, root, "Pending");
  el("text", { x: 1000, y: 182, class: "hv-t hv-s hv-dim" }, root, "mass · aluminium 6061");
  const massVal = el("text", { x: 1000, y: 204, class: "hv-t" }, root, "");
  el("text", { x: 1000, y: 238, class: "hv-t hv-s hv-dim" }, root, "re-checks on every edit");

  // viewport badge (top centre, like the live receipt badge)
  const badge = el("g", { opacity: 0 }, root);
  const badgeBg = el("rect", { x: 480, y: 96, width: 240, height: 28, rx: 14, fill: "none", "stroke-width": 1.5 }, badge);
  const badgeTxt = el("text", { x: 600, y: 115, "text-anchor": "middle", class: "hv-t hv-b" }, badge, "");

  // ---- parts -------------------------------------------------------------
  const scene = el("g", {}, root);
  const base = boxFaces(-80, -50, 0, 160, 100, BASE_H);
  const baseTop = el("polygon", { points: base.top, fill: "#c8ccd3" }, scene);
  el("polygon", { points: base.right, fill: "#7d828b" }, scene);
  el("polygon", { points: base.front, fill: "#9aa0a9" }, scene);
  const faceHi = el("polygon", { points: pts([[-20, -20, BASE_H], [20, -20, BASE_H], [20, 20, BASE_H], [-20, 20, BASE_H]]), fill: "#f08a24", opacity: 0 }, scene);

  const boss = el("g", { opacity: 0 }, scene);
  const bossTop = el("polygon", { fill: "#f5a04b" }, boss);
  const bossRight = el("polygon", { fill: "#b8661b" }, boss);
  const bossFront = el("polygon", { fill: "#d67d27" }, boss);

  const cov = boxFaces(-60, -35, COVER_Z, 120, 70, 6);
  const coverG = el("g", { opacity: 0.62 }, scene);
  el("polygon", { points: cov.front, fill: "#7a5f22" }, coverG);
  el("polygon", { points: cov.right, fill: "#5d4818" }, coverG);
  el("polygon", { points: cov.top, fill: "#b38d3a" }, coverG);
  // cover underside outline so the gap reads
  el("polygon", { points: pts([[-60, -35, COVER_Z], [60, -35, COVER_Z], [60, 35, COVER_Z], [-60, 35, COVER_Z]]), fill: "none", stroke: "#d9b45a", "stroke-opacity": 0.5, "stroke-dasharray": "4 4" }, coverG);

  // press-pull arrow + live dimension
  const handle = el("g", { opacity: 0, class: "hv-handle" }, root);
  const hLine = el("line", { stroke: "#3d8bff", "stroke-width": 3 }, handle);
  const hHead = el("polygon", { fill: "#3d8bff" }, handle);
  const hHit = el("circle", { r: 26, fill: "transparent" }, handle);
  const dim = el("g", { opacity: 0 }, root);
  const dimBg = el("rect", { width: 108, height: 26, rx: 6, fill: "#1f1f22", stroke: "#3d8bff" }, dim);
  const dimTxt = el("text", { class: "hv-t hv-b", "text-anchor": "middle" }, dim, "");

  // the agent lives in YOUR client (a terminal here), not inside vcad.app:
  // it drives the app over local MCP. Drawn as a separate window on top.
  const prompt = el("g", { opacity: 0 }, svg);
  el("rect", { x: 300, y: 560, width: 600, height: 112, rx: 10, fill: "#0e0e10", stroke: "#4a4a4f" }, prompt);
  el("path", { d: "M310 560 h580 a10 10 0 0 1 10 10 v18 h-600 v-18 a10 10 0 0 1 10 -10 z", fill: "#1c1c1f" }, prompt);
  [["#ff5f57", 318], ["#febc2e", 334], ["#28c840", 350]].forEach(([c, x]) => el("circle", { cx: x, cy: 574, r: 4.5, fill: c }, prompt));
  el("text", { x: 600, y: 578, "text-anchor": "middle", class: "hv-m hv-dim" }, prompt, "terminal — your agent, over local MCP");
  const promptTxt = el("text", { x: 318, y: 614, class: "hv-m" }, prompt, "");
  const replyTxt = el("text", { x: 318, y: 644, class: "hv-m hv-ok" }, prompt, "");

  // toolbar
  el("rect", { x: 380, y: 690, width: 440, height: 44, rx: 22, class: "hv-panel" }, root);
  el("text", { x: 410, y: 717, class: "hv-t hv-b" }, root, "Design");
  for (let i = 0; i < 9; i++) el("rect", { x: 490 + i * 34, y: 702, width: 18, height: 18, rx: 4, fill: "none", stroke: "#9a9aa0", "stroke-width": 1.4 }, root);

  // agent cursor
  const cursor = el("g", { opacity: 0, transform: "translate(900 560)" }, root);
  el("path", { d: "M0 0 L0 22 L6 16 L10 26 L14 24 L10 14 L18 14 Z", fill: "#ff2052", stroke: "#fff", "stroke-width": 1.5 }, cursor);
  el("rect", { x: 18, y: 22, width: 58, height: 20, rx: 10, fill: "#ff2052" }, cursor);
  el("text", { x: 47, y: 36, "text-anchor": "middle", class: "hv-t hv-s hv-w" }, cursor, "agent");

  // ---- state + render ----------------------------------------------------
  let h = 0, checked = false;
  function render() {
    const f = boxFaces(-20, -20, BASE_H, 40, 40, Math.max(h, 0.01));
    bossTop.setAttribute("points", f.top);
    bossRight.setAttribute("points", f.right);
    bossFront.setAttribute("points", f.front);
    const [tx, ty] = P(0, 0, BASE_H + h);
    hLine.setAttribute("x1", tx); hLine.setAttribute("y1", ty);
    hLine.setAttribute("x2", tx); hLine.setAttribute("y2", ty - 46);
    hHead.setAttribute("points", `${tx - 8},${ty - 44} ${tx + 8},${ty - 44} ${tx},${ty - 60}`);
    hHit.setAttribute("cx", tx); hHit.setAttribute("cy", ty - 40);
    const [dx, dy] = P(20, 20, BASE_H + h / 2);
    dimBg.setAttribute("x", dx + 26); dimBg.setAttribute("y", dy - 13);
    dimTxt.setAttribute("x", dx + 80); dimTxt.setAttribute("y", dy + 5);
    dimTxt.textContent = `h ${h.toFixed(1)} mm`;
    navBossDim.textContent = `${h.toFixed(1)} mm`;
    massVal.textContent = `${massG(h).toFixed(0)} g`;
    if (checked) setReceipt();
  }
  function setReceipt() {
    const c = clearance(h), ok = c >= MIN_CLEAR;
    const col = ok ? "#3ecf8e" : "#ff4d5e";
    recVal.textContent = c >= 0 ? `${c.toFixed(2)} mm` : `${(-c).toFixed(2)} mm into`;
    recVal.setAttribute("fill", col);
    recState.textContent = ok ? "Holds" : "Violated";
    recState.setAttribute("fill", col);
    badgeBg.setAttribute("stroke", col);
    badgeTxt.setAttribute("fill", col);
    badgeTxt.textContent = `${ok ? "✓" : "✕"} clearance ≥ 3 mm · ${c.toFixed(2)} mm`;
  }

  // ---- animation helpers ---------------------------------------------------
  let run = 0;
  const alive = (id) => id === run;
  const sleep = (ms) => new Promise((r) => setTimeout(r, reduced ? 0 : ms));
  const ease = (t) => (t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
  function tween(ms, fn) {
    if (reduced) { fn(1); return Promise.resolve(); }
    return new Promise((res) => {
      const t0 = performance.now();
      const step = (now) => {
        const t = Math.min(1, (now - t0) / ms);
        fn(ease(t));
        t < 1 ? requestAnimationFrame(step) : res();
      };
      requestAnimationFrame(step);
    });
  }
  let cur = [900, 560];
  const moveCursor = (to, ms) => {
    const from = cur.slice();
    return tween(ms, (t) => {
      cur = [from[0] + (to[0] - from[0]) * t, from[1] + (to[1] - from[1]) * t];
      cursor.setAttribute("transform", `translate(${cur[0]} ${cur[1]})`);
    });
  };
  const show = (n, on = true) => n.setAttribute("opacity", on ? 1 : 0);
  const hint = document.getElementById("heroHint");

  async function play() {
    const id = ++run;
    h = 0; checked = false; render();
    [pill, badge, navBoss, boss, handle, dim, cursor].forEach((n) => show(n, false));
    faceHi.setAttribute("opacity", 0);
    recVal.textContent = "—"; recVal.removeAttribute("fill");
    recState.textContent = "Pending"; recState.setAttribute("fill", "#8a8a90");
    cur = [900, 560]; cursor.setAttribute("transform", "translate(900 560)");
    hint.textContent = "Illustration of vcad.app";
    svg.classList.remove("hv-live");

    show(prompt);
    const ask = "> add a boss under the cover, keep 3 mm clearance";
    promptTxt.textContent = ""; replyTxt.textContent = "";
    if (reduced) promptTxt.textContent = ask;
    else for (const ch of ask) { if (!alive(id)) return; promptTxt.textContent += ch; await sleep(28); }
    await sleep(400); if (!alive(id)) return;

    pillTxt.textContent = "agent · apply_edits"; show(pill);
    show(cursor);
    const [fx, fy] = P(0, 0, BASE_H);
    await moveCursor([fx + 4, fy - 4], 900); if (!alive(id)) return;
    faceHi.setAttribute("opacity", 0.9);
    await sleep(350); if (!alive(id)) return;

    faceHi.setAttribute("opacity", 0);
    show(boss); show(navBoss); show(handle); show(dim);
    const [, startY] = P(0, 0, BASE_H);
    await tween(1400, (t) => {
      h = AGENT_H * t; render();
      cursor.setAttribute("transform", `translate(${fx + 4} ${startY - 44 - h * S})`);
    });
    cur = [fx + 4, startY - 44 - h * S];
    if (!alive(id)) return;

    pillTxt.textContent = "agent · inspect_cad · checking…";
    await sleep(700); if (!alive(id)) return;
    checked = true; setReceipt(); show(badge);
    pillTxt.textContent = "agent · done · 1 claim holds";
    replyTxt.textContent = "✓ boss 26 mm · clearance 4.00 mm (holds)";
    await moveCursor([860, 600], 800); if (!alive(id)) return;
    show(cursor, false);
    await sleep(600); if (!alive(id)) return;
    show(pill, false);
    goLive();
  }
  function goLive() {
    checked = true; setReceipt(); render();
    if (!replyTxt.textContent) { promptTxt.textContent = "> add a boss under the cover, keep 3 mm clearance"; replyTxt.textContent = "✓ boss 26 mm · clearance 4.00 mm (holds)"; }
    show(prompt);
    [boss, navBoss, handle, dim, badge].forEach((n) => show(n));
    svg.classList.add("hv-live");
    hint.textContent = "Your turn: drag the blue arrow";
  }

  // ---- interaction ---------------------------------------------------------
  const toSvgY = (e) => {
    const pt = svg.createSVGPoint(); pt.x = e.clientX; pt.y = e.clientY;
    return pt.matrixTransform(svg.getScreenCTM().inverse()).y;
  };
  let drag = null;
  handle.addEventListener("pointerdown", (e) => {
    if (!svg.classList.contains("hv-live")) { run++; goLive(); }
    drag = { y: toSvgY(e), h };
    prompt.setAttribute("opacity", 0.35); // your edit now, not the agent's
    handle.setPointerCapture(e.pointerId);
    e.preventDefault();
  });
  handle.addEventListener("pointermove", (e) => {
    if (!drag) return;
    h = Math.max(2, Math.min(H_MAX, drag.h - (toSvgY(e) - drag.y) / S));
    render();
  });
  const end = () => { drag = null; };
  handle.addEventListener("pointerup", end);
  handle.addEventListener("pointercancel", end);
  // keyboard: arrow keys nudge the boss once live
  handle.setAttribute("tabindex", "0");
  handle.setAttribute("role", "slider");
  handle.setAttribute("aria-label", "Boss height");
  handle.addEventListener("keydown", (e) => {
    const d = { ArrowUp: 1, ArrowRight: 1, ArrowDown: -1, ArrowLeft: -1 }[e.key];
    if (!d) return;
    if (!svg.classList.contains("hv-live")) { run++; goLive(); }
    prompt.setAttribute("opacity", 0.35);
    h = Math.max(2, Math.min(H_MAX, h + d)); render(); e.preventDefault();
  });

  document.getElementById("heroReplay")?.addEventListener("click", play);

  // phones: crop to the model, badge and prompt (the side panels would be unreadable)
  const mq = matchMedia("(max-width: 600px)");
  const fit = () => svg.setAttribute("viewBox", mq.matches ? "270 46 660 714" : "0 0 1200 760");
  fit(); mq.addEventListener("change", fit);

  // start when visible
  render();
  const io = new IntersectionObserver((es) => {
    if (es.some((x) => x.isIntersecting)) { io.disconnect(); play(); }
  }, { threshold: 0.3 });
  io.observe(svg);
}
