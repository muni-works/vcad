// Agent section figure: a live receipt. A small part on the left, its
// vcad.receipt/1 on the right. Claims check one by one; then you press-pull
// the boss, the clearance claim goes Stale, re-checks to Violated, the agent
// fixes it, and it holds again. Theme colours come from home.css (il-*, rc-*).

const svg = document.getElementById("receiptArt");
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

  // ---- part (mm), isometric ------------------------------------------------
  const BASE_H = 10, COVER_Z = 40, OK_H = 26, BAD_H = 32;
  const S = 1.75, CX = 142, CY = 226, C30 = Math.cos(Math.PI / 6);
  const P = (x, y, z) => [CX + (x - y) * C30 * S, CY + (x + y) * 0.5 * S - z * S];
  const pts = (a) => a.map((p) => P(...p).map((v) => v.toFixed(1)).join(",")).join(" ");
  const faces = (x, y, z, w, d, h) => ({
    top: pts([[x, y, z + h], [x + w, y, z + h], [x + w, y + d, z + h], [x, y + d, z + h]]),
    right: pts([[x + w, y, z], [x + w, y + d, z], [x + w, y + d, z + h], [x + w, y, z + h]]),
    front: pts([[x, y + d, z], [x + w, y + d, z], [x + w, y + d, z + h], [x, y + d, z + h]]),
  });

  const part = el("g");
  for (let i = -70; i <= 70; i += 14) {
    const [a, b] = [P(i, -60, 0), P(i, 60, 0)], [c, d] = [P(-70, i * 6 / 7, 0), P(70, i * 6 / 7, 0)];
    el("line", { class: "il-grid", x1: a[0], y1: a[1], x2: b[0], y2: b[1] }, part);
    el("line", { class: "il-grid", x1: c[0], y1: c[1], x2: d[0], y2: d[1] }, part);
  }
  const base = faces(-60, -40, 0, 120, 80, BASE_H);
  el("polygon", { class: "il-front", points: base.front }, part);
  el("polygon", { class: "il-right", points: base.right }, part);
  el("polygon", { class: "il-top", points: base.top }, part);
  const bossFront = el("polygon", { class: "il-front" }, part);
  const bossRight = el("polygon", { class: "il-right" }, part);
  const bossTop = el("polygon", { class: "il-sel" }, part);
  const cov = faces(-48, -30, COVER_Z, 96, 60, 5);
  const cover = el("g", { class: "rc-cover" }, part);
  el("polygon", { points: cov.front }, cover);
  el("polygon", { points: cov.right }, cover);
  el("polygon", { points: cov.top }, cover);
  el("polygon", { class: "rc-under", points: pts([[-48, -30, COVER_Z], [48, -30, COVER_Z], [48, 30, COVER_Z], [-48, 30, COVER_Z]]) }, part);
  // clearance gauge between boss top and cover underside
  const gauge = el("g", { class: "rc-gauge" }, part);
  const gLine = el("line", {}, gauge);
  const gT1 = el("line", {}, gauge), gT2 = el("line", {}, gauge);
  const gTxt = el("text", { class: "il-t", "text-anchor": "start" }, gauge);

  // event line under the part
  const evWho = el("text", { class: "il-t rc-who", x: 24, y: 392 }, svg, "");
  const evWhat = el("text", { class: "il-t il-fg", x: 24, y: 410 }, svg, "");

  // ---- receipt paper -----------------------------------------------------------
  const RX = 290, RW = 214, RY = 28;
  const rows = [
    { name: "clearance ≥ 3 mm", val: "4.00 mm" },
    { name: "mass ≤ 900 g", val: "804 g" },
    { name: "fits P1S bed", val: "120 × 80" },
    { name: "first mode ≥ 400 Hz", val: "612 Hz" },
  ];
  const ROW0 = RY + 92, RH = 56, RB = ROW0 + rows.length * RH + 70;
  // paper with a torn bottom edge
  let edge = `M${RX} ${RY} H${RX + RW} V${RB}`;
  for (let x = RX + RW; x > RX; x -= 10.7) edge += ` L${(x - 5.35).toFixed(1)} ${RB + 6} L${(x - 10.7).toFixed(1)} ${RB}`;
  edge += " Z";
  el("path", { class: "rc-paper", d: edge });
  el("text", { class: "il-t rc-cap", x: RX + 16, y: RY + 26 }, svg, "VCAD.RECEIPT/1");
  el("text", { class: "il-t il-fg rc-title", x: RX + 16, y: RY + 50 }, svg, "bracket-cover");
  el("line", { class: "rc-perf", x1: RX + 16, y1: RY + 66, x2: RX + RW - 16, y2: RY + 66 });
  const ui = rows.map((r, i) => {
    const y = ROW0 + i * RH;
    const g = el("g", {});
    el("text", { class: "il-t il-fg", x: RX + 16, y }, g, r.name);
    const val = el("text", { class: "il-t", x: RX + 16, y: y + 18 }, g, "—");
    const pill = el("rect", { class: "rc-pill", x: RX + RW - 86, y: y + 5, width: 70, height: 18 }, g);
    const st = el("text", { class: "il-t rc-st", x: RX + RW - 51, y: y + 18, "text-anchor": "middle" }, g, "PENDING");
    if (i < rows.length - 1) el("line", { class: "rc-perf", x1: RX + 16, y1: y + 36, x2: RX + RW - 16, y2: y + 36 }, g);
    return { val, pill, st };
  });
  el("line", { class: "rc-perf", x1: RX + 16, y1: RB - 52, x2: RX + RW - 16, y2: RB - 52 });
  const sum = el("text", { class: "il-t il-fg", x: RX + 16, y: RB - 30 }, svg, "");
  const hash = el("text", { class: "il-t", x: RX + 16, y: RB - 12 }, svg, "");

  // ---- state -------------------------------------------------------------------
  let h = OK_H;
  const clr = () => COVER_Z - (BASE_H + h);
  function drawPart(state) {
    const f = faces(-15, -15, BASE_H, 30, 30, h);
    bossFront.setAttribute("points", f.front);
    bossRight.setAttribute("points", f.right);
    bossTop.setAttribute("points", f.top);
    bossTop.setAttribute("class", state === "violated" ? "rc-bad" : "il-sel");
    const [x0, y0] = P(15, 15, BASE_H + h), [, y1] = P(15, 15, COVER_Z);
    const gx = x0 + 10;
    gLine.setAttribute("x1", gx); gLine.setAttribute("x2", gx);
    gLine.setAttribute("y1", y0); gLine.setAttribute("y2", y1);
    gT1.setAttribute("x1", gx - 4); gT1.setAttribute("x2", gx + 4); gT1.setAttribute("y1", y0); gT1.setAttribute("y2", y0);
    gT2.setAttribute("x1", gx - 4); gT2.setAttribute("x2", gx + 4); gT2.setAttribute("y1", y1); gT2.setAttribute("y2", y1);
    gTxt.setAttribute("x", gx + 8); gTxt.setAttribute("y", (y0 + y1) / 2 + 4);
    gTxt.textContent = "";
    gauge.setAttribute("class", `rc-gauge rc-${state || "holds"}`);
  }
  const LABEL = { pending: "PENDING", checking: "CHECKING", holds: "HOLDS", stale: "STALE", violated: "VIOLATED" };
  function setRow(i, state, val) {
    const r = ui[i];
    if (val != null) r.val.textContent = val;
    r.pill.setAttribute("class", `rc-pill rc-${state}`);
    r.st.setAttribute("class", `il-t rc-st rc-${state}`);
    r.st.textContent = LABEL[state];
  }
  function setSummary(n, ok, hex) {
    sum.textContent = ok ? `✓ ${n} of 4 claims hold` : `✕ ${n} of 4 claims hold`;
    sum.setAttribute("class", `il-t ${ok ? "rc-holds" : "rc-violated"}`);
    hash.textContent = `sha256 ${hex}`;
  }
  const say = (who, what) => { evWho.textContent = who; evWhat.textContent = what; };

  // ---- timeline ------------------------------------------------------------------
  let run = 0;
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const tween = (ms, fn) => new Promise((res) => {
    const t0 = performance.now();
    const step = (now) => {
      const t = Math.min(1, (now - t0) / ms);
      fn(t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2);
      t < 1 ? requestAnimationFrame(step) : res();
    };
    requestAnimationFrame(step);
  });

  function finalState() {
    h = OK_H; drawPart("holds");
    rows.forEach((r, i) => setRow(i, "holds", r.val));
    setSummary(4, true, "3f9a…c21e");
    say("agent · check_clearance", "clearance 4.00 mm · holds");
  }

  async function play() {
    const id = ++run;
    const alive = () => id === run;
    h = OK_H; drawPart("pending");
    rows.forEach((_, i) => setRow(i, "pending", "—"));
    sum.textContent = ""; hash.textContent = "";
    say("agent · build_receipt", "checking 4 claims…");
    await sleep(700);
    for (let i = 0; i < rows.length; i++) {
      if (!alive()) return;
      setRow(i, "checking");
      await sleep(420);
      setRow(i, "holds", rows[i].val);
      if (i === 0) drawPart("holds");
    }
    setSummary(4, true, "3f9a…c21e");
    say("agent · build_receipt", "receipt sealed · 4 claims hold");
    await sleep(2200); if (!alive()) return;

    say("you · press-pull", "boss +6 mm");
    setRow(0, "stale"); setSummary(3, false, "3f9a…c21e");
    await tween(900, (t) => { h = OK_H + (BAD_H - OK_H) * t; drawPart("stale"); ui[0].val.textContent = `${clr().toFixed(2)} mm`; });
    setRow(0, "stale"); setSummary(3, false, "3f9a…c21e");
    await sleep(900); if (!alive()) return;
    setRow(0, "checking"); say("vcad · re-check", "boss changed · re-checking");
    await sleep(600); if (!alive()) return;
    setRow(0, "violated", `${clr().toFixed(2)} mm`); drawPart("violated"); setSummary(3, false, "b04d…7a19");
    say("vcad · re-check", "clearance violated");
    await sleep(1800); if (!alive()) return;

    say("agent · apply_edits", "boss h 32 → 26");
    setRow(0, "stale");
    await tween(900, (t) => { h = BAD_H + (OK_H - BAD_H) * t; drawPart("stale"); ui[0].val.textContent = `${clr().toFixed(2)} mm`; });
    setRow(0, "checking");
    await sleep(600); if (!alive()) return;
    setRow(0, "holds", `${clr().toFixed(2)} mm`); drawPart("holds"); setSummary(4, true, "e71c…05b8");
    say("agent · check_clearance", "clearance 4.00 mm · re-sealed");
    await sleep(3800); if (!alive()) return;
    play();
  }

  if (reduced) { finalState(); return; }
  finalState();
  const io = new IntersectionObserver((es) => {
    for (const e of es) { if (e.isIntersecting) play(); else run++; }
  }, { threshold: 0.35 });
  io.observe(svg);
}
