/**
 * Winding continuity check: does every coil of a PCB motor winding actually
 * carry current, in the right direction?
 *
 * DRC answers "is the copper legal?" — no shorts, no clearance faults, every
 * net one island. It does not answer "does current flow through the coils?".
 * A spiral whose inner via has nothing on the return layer is a dead-end stub
 * hanging off a perfectly connected net: DRC-clean, electrically absent. The
 * pcb-motor stator v3 shipped that way (2 of 9 coils open) with DRC 0.
 *
 * Method: treat the copper as a resistor network and solve it. For each phase,
 * 1 A goes into the phase's pad and the neutral is held at 0 V; every coil's
 * signed ampere-turns are then read off the spiral layer as
 * Σ I_seg · Δθ_seg / 2π (the copper's circulation around the coil centre).
 * A healthy coil reads ≈ ±turns per amp with the sign its plan polarity says;
 * a dead coil reads 0, a partly shorted coil reads low, a reversed coil reads
 * the wrong sign. Copper of the fed net that carries no current (a stub) is
 * reported too.
 *
 * The network is deliberately conservative about what conducts: trace
 * endpoints within MERGE_TOL join, an endpoint landing on another same-layer
 * trace's body splits it (a T-junction), and a via or pad joins every trace
 * endpoint inside its copper on every layer it spans. Crossings with no
 * endpoint at the contact, and traces that pass through a via without ending
 * there, do NOT conduct here. That direction can only report a coil dead that
 * is alive, never the reverse — shorts and bypasses are DRC's job
 * (Short / SameNetBypass), which this check complements, not replaces.
 */
import type { Footprint, Pad, Pcb, Vec2 } from "@vcad/ir";

/** One coil of the plan, as the check needs it. */
export interface WindingCoilSpec {
  /** Coil centre (board mm). */
  center: Vec2;
  /** Phase net the coil belongs to. */
  net: string;
  /** Plan polarity (±1): coils of opposite polarity must circulate oppositely. */
  polarity: 1 | -1;
}

/** What to check. */
export interface WindingCheckSpec {
  coils: WindingCoilSpec[];
  /** Spiral outer radius (mm) — the circulation window around each centre. */
  outerRadius: number;
  /** Turns per coil — the expected |ampere-turns| per amp. */
  turns: number;
  /** Layer the spirals are on (circulation is read there only). */
  copperLayer: string;
  /** Wye neutral net: all copper of this net is held at 0 V. */
  neutralNet: string;
  /** Accepted |AT|/turns band (default 0.9–1.1). */
  tolerance?: number;
  /** Stub copper of a fed net above this length fails the check (default 1 mm). */
  maxDeadCopperMm?: number;
}

/** Per-coil result. */
export interface CoilReading {
  center: Vec2;
  net: string;
  polarity: 1 | -1;
  /** Signed ampere-turns with 1 A into the coil's own phase pad. */
  ampereTurns: number;
  /** True when |AT| is within tolerance of `turns` with the winding's common sense. */
  ok: boolean;
  /** Why not, when `ok` is false. */
  problem?: string;
}

/** Per-phase result. */
export interface PhaseReading {
  net: string;
  /** Phase pad → neutral resistance (Ω, 1 A in). `null` when there is no path. */
  resistanceOhm: number | null;
  /** Length (mm) of this net's copper carrying < 0.1% of the feed current. */
  deadCopperMm: number;
  problem?: string;
}

export interface WindingCheckResult {
  pass: boolean;
  /** Common circulation sense: +1 when polarity-+1 coils circulate CCW. */
  sense: 1 | -1;
  coils: CoilReading[];
  phases: PhaseReading[];
  /** Human-readable reasons, one per failure. Empty on pass. */
  failures: string[];
}

const MERGE_TOL = 0.02; // mm — endpoints closer than this are one node
const SHEET_RES = 1.7e-8 / 70e-6; // Ω/□ for 2 oz copper; only ratios matter for AT
const JOIN_G = 1e6; // via barrels / pad copper: effectively ideal

function padWorld(fp: Footprint, pad: Pad): Vec2 {
  const a = ((fp.rotation ?? 0) * Math.PI) / 180;
  const c = Math.cos(a);
  const s = Math.sin(a);
  return {
    x: fp.position.x + pad.position.x * c - pad.position.y * s,
    y: fp.position.y + pad.position.x * s + pad.position.y * c,
  };
}

function padRadius(pad: Pad): number {
  const s = pad.shape as {
    type: string;
    diameter?: number;
    width?: number;
    height?: number;
  };
  if (s.type === "Circle") return (s.diameter ?? 1) / 2;
  return Math.min(s.width ?? 1, s.height ?? 1) / 2;
}

interface Edge {
  a: number;
  b: number;
  g: number;
  /** Index into pcb.traces for trace pieces, -1 for joins. */
  trace: number;
  len: number;
}

/** Build the copper network once; it is shared by every phase solve. */
function buildNetwork(pcb: Pcb) {
  const nodeLayer: string[] = [];
  const nodePos: Vec2[] = [];
  const grid = new Map<string, number[]>();
  const key = (l: string, x: number, y: number) => `${l}|${x}|${y}`;
  const node = (layer: string, p: Vec2): number => {
    const gx = Math.round(p.x / MERGE_TOL);
    const gy = Math.round(p.y / MERGE_TOL);
    for (let dx = -1; dx <= 1; dx++) {
      for (let dy = -1; dy <= 1; dy++) {
        for (const n of grid.get(key(layer, gx + dx, gy + dy)) ?? []) {
          if (Math.hypot(nodePos[n]!.x - p.x, nodePos[n]!.y - p.y) <= MERGE_TOL)
            return n;
        }
      }
    }
    const id = nodePos.length;
    nodeLayer.push(layer);
    nodePos.push({ x: p.x, y: p.y });
    const k = key(layer, gx, gy);
    const arr = grid.get(k) ?? [];
    arr.push(id);
    grid.set(k, arr);
    return id;
  };

  const ends = pcb.traces.map((t) => [
    node(t.layer, t.start),
    node(t.layer, t.end),
  ]);

  // T-junctions: a node lying on another same-layer trace's body splits it.
  const CELL = 1;
  const cellKey = (l: string, x: number, y: number) =>
    `${l}|${Math.floor(x / CELL)}|${Math.floor(y / CELL)}`;
  const traceCells = new Map<string, number[]>();
  pcb.traces.forEach((t, i) => {
    const x0 = Math.min(t.start.x, t.end.x) - t.width;
    const x1 = Math.max(t.start.x, t.end.x) + t.width;
    const y0 = Math.min(t.start.y, t.end.y) - t.width;
    const y1 = Math.max(t.start.y, t.end.y) + t.width;
    for (let x = Math.floor(x0 / CELL); x <= Math.floor(x1 / CELL); x++) {
      for (let y = Math.floor(y0 / CELL); y <= Math.floor(y1 / CELL); y++) {
        const k = `${t.layer}|${x}|${y}`;
        const arr = traceCells.get(k) ?? [];
        arr.push(i);
        traceCells.set(k, arr);
      }
    }
  });
  const splits: Array<Array<{ t: number; n: number }>> = pcb.traces.map(
    () => [],
  );
  for (let n = 0; n < nodePos.length; n++) {
    const p = nodePos[n]!;
    for (const i of traceCells.get(cellKey(nodeLayer[n]!, p.x, p.y)) ?? []) {
      const [a, b] = ends[i]!;
      if (n === a || n === b) continue;
      const t = pcb.traces[i]!;
      const dx = t.end.x - t.start.x;
      const dy = t.end.y - t.start.y;
      const l2 = dx * dx + dy * dy;
      if (l2 === 0) continue;
      const u = ((p.x - t.start.x) * dx + (p.y - t.start.y) * dy) / l2;
      if (u <= 0 || u >= 1) continue;
      if (
        Math.hypot(t.start.x + u * dx - p.x, t.start.y + u * dy - p.y) <
        t.width / 2
      ) {
        splits[i]!.push({ t: u, n });
      }
    }
  }

  const edges: Edge[] = [];
  pcb.traces.forEach((t, i) => {
    const chain = [
      ends[i]![0]!,
      ...splits[i]!.sort((p, q) => p.t - q.t).map((s) => s.n),
      ends[i]![1]!,
    ];
    for (let k = 0; k + 1 < chain.length; k++) {
      const a = chain[k]!;
      const b = chain[k + 1]!;
      if (a === b) continue;
      const len = Math.hypot(
        nodePos[a]!.x - nodePos[b]!.x,
        nodePos[a]!.y - nodePos[b]!.y,
      );
      edges.push({
        a,
        b,
        g: t.width / (SHEET_RES * Math.max(len, 1e-4)),
        trace: i,
        len,
      });
    }
  });

  // Vias and pads: join every node inside their copper, on any layer they span.
  // `joinOf` remembers which join a node fell in, so a trace piece lying
  // wholly inside one pad (shunted by the ideal pad copper) isn't miscounted
  // as a dead stub.
  const joinOf = new Map<number, number>();
  let joins = 0;
  const join = (p: Vec2, r: number, layers: Set<string> | null): number[] => {
    const inside: number[] = [];
    for (let n = 0; n < nodePos.length; n++) {
      if (layers && !layers.has(nodeLayer[n]!)) continue;
      if (Math.hypot(nodePos[n]!.x - p.x, nodePos[n]!.y - p.y) <= r)
        inside.push(n);
    }
    for (let k = 1; k < inside.length; k++) {
      edges.push({
        a: inside[0]!,
        b: inside[k]!,
        g: JOIN_G,
        trace: -1,
        len: 0,
      });
    }
    const id = joins++;
    for (const n of inside) if (!joinOf.has(n)) joinOf.set(n, id);
    return inside;
  };
  for (const v of pcb.vias) join(v.position, v.diameter / 2, null);
  const padNodes = new Map<string, number[]>(); // net → nodes touching its pads
  for (const fp of pcb.footprints) {
    for (const pad of fp.pads) {
      const layers = new Set<string>(pad.layers as string[]);
      const inside = join(padWorld(fp, pad), padRadius(pad), layers);
      if (!pad.net) continue;
      const arr = padNodes.get(pad.net) ?? [];
      arr.push(...inside);
      padNodes.set(pad.net, arr);
    }
  }

  const adj: Array<Array<[number, number]>> = nodePos.map(() => []);
  for (const e of edges) {
    adj[e.a]!.push([e.b, e.g]);
    adj[e.b]!.push([e.a, e.g]);
  }
  return { nodeLayer, nodePos, edges, adj, padNodes, joinOf };
}

/** Solve L·V = I on the component of `src`, with `ground` nodes at 0 V. */
function solve(
  adj: Array<Array<[number, number]>>,
  src: number,
  ground: Set<number>,
): Float64Array | null {
  const n = adj.length;
  const seen = new Uint8Array(n);
  const stack = [src];
  seen[src] = 1;
  let reachesGround = ground.has(src);
  const free: number[] = [];
  while (stack.length) {
    const u = stack.pop()!;
    if (ground.has(u)) {
      reachesGround = true;
      continue;
    }
    free.push(u);
    for (const [v] of adj[u]!) {
      if (!seen[v]) {
        seen[v] = 1;
        stack.push(v);
      }
    }
  }
  if (!reachesGround) return null;
  const V = new Float64Array(n);
  const isFree = new Uint8Array(n);
  for (const i of free) isFree[i] = 1;
  const diag = new Float64Array(n);
  for (const i of free) for (const [, g] of adj[i]!) diag[i]! += g;
  const apply = (x: Float64Array, out: Float64Array) => {
    for (const i of free) {
      let s = diag[i]! * x[i]!;
      for (const [j, g] of adj[i]!) if (isFree[j]) s -= g * x[j]!;
      out[i] = s;
    }
  };
  // Jacobi-preconditioned conjugate gradients.
  const r = new Float64Array(n);
  r[src] = 1;
  const z = new Float64Array(n);
  const p = new Float64Array(n);
  const Ap = new Float64Array(n);
  for (const i of free) z[i] = r[i]! / diag[i]!;
  p.set(z);
  let rz = 0;
  for (const i of free) rz += r[i]! * z[i]!;
  for (let it = 0; it < 50 * free.length + 100; it++) {
    apply(p, Ap);
    let pAp = 0;
    for (const i of free) pAp += p[i]! * Ap[i]!;
    const alpha = rz / pAp;
    let rr = 0;
    for (const i of free) {
      V[i]! += alpha * p[i]!;
      r[i]! -= alpha * Ap[i]!;
      rr += r[i]! * r[i]!;
    }
    if (Math.sqrt(rr) < 1e-12) break;
    let rz2 = 0;
    for (const i of free) {
      z[i] = r[i]! / diag[i]!;
      rz2 += r[i]! * z[i]!;
    }
    const beta = rz2 / rz;
    rz = rz2;
    for (const i of free) p[i] = z[i]! + beta * p[i]!;
  }
  return V;
}

/** Signed angle swept from a to b around c, in (−π, π]. */
function sweep(c: Vec2, a: Vec2, b: Vec2): number {
  const t0 = Math.atan2(a.y - c.y, a.x - c.x);
  const t1 = Math.atan2(b.y - c.y, b.x - c.x);
  let d = t1 - t0;
  while (d > Math.PI) d -= 2 * Math.PI;
  while (d <= -Math.PI) d += 2 * Math.PI;
  return d;
}

/**
 * Check that every coil carries its phase current with the planned sign.
 * Fails closed: no phase pad, no neutral copper, no path, or a coil out of
 * band are all failures.
 */
export function checkWindingContinuity(
  pcb: Pcb,
  spec: WindingCheckSpec,
): WindingCheckResult {
  const tol = spec.tolerance ?? 0.1;
  const maxDead = spec.maxDeadCopperMm ?? 1;
  const net = buildNetwork(pcb);
  const failures: string[] = [];
  const ground = new Set<number>();
  // Every node on a neutral-net trace (ends and T-splits) is the star.
  for (const e of net.edges) {
    if (e.trace >= 0 && pcb.traces[e.trace]!.net === spec.neutralNet) {
      ground.add(e.a);
      ground.add(e.b);
    }
  }
  for (const n of net.padNodes.get(spec.neutralNet) ?? []) ground.add(n);
  if (ground.size === 0)
    failures.push(`no copper or pad on neutral net '${spec.neutralNet}'`);

  // Spiral-layer trace pieces inside each coil's window, for circulation.
  const window = spec.outerRadius + 0.5;
  const coilEdges = spec.coils.map((c) =>
    net.edges.filter((e) => {
      if (e.trace < 0 || net.nodeLayer[e.a] !== spec.copperLayer) return false;
      const pa = net.nodePos[e.a]!;
      const pb = net.nodePos[e.b]!;
      const ra = Math.hypot(pa.x - c.center.x, pa.y - c.center.y);
      const rb = Math.hypot(pb.x - c.center.x, pb.y - c.center.y);
      // Segments touching the centre (the inner lead-in) have no defined sweep.
      return ra <= window && rb <= window && ra > 0.05 && rb > 0.05;
    }),
  );

  const coils: CoilReading[] = spec.coils.map((c) => ({
    center: c.center,
    net: c.net,
    polarity: c.polarity,
    ampereTurns: 0,
    ok: false,
  }));
  const phases: PhaseReading[] = [];
  const phaseNets = [...new Set(spec.coils.map((c) => c.net))];

  for (const ph of phaseNets) {
    const pads = net.padNodes.get(ph) ?? [];
    if (pads.length === 0) {
      phases.push({
        net: ph,
        resistanceOhm: null,
        deadCopperMm: 0,
        problem: "no pad on this net",
      });
      failures.push(`phase ${ph}: no pad to feed`);
      continue;
    }
    const V = ground.size ? solve(net.adj, pads[0]!, ground) : null;
    if (!V) {
      phases.push({
        net: ph,
        resistanceOhm: null,
        deadCopperMm: 0,
        problem: "no path to neutral",
      });
      failures.push(
        `phase ${ph}: no copper path from its pad to neutral '${spec.neutralNet}'`,
      );
      continue;
    }
    const current = (e: Edge) => e.g * (V[e.a]! - V[e.b]!);
    spec.coils.forEach((c, k) => {
      if (c.net !== ph) return;
      let at = 0;
      for (const e of coilEdges[k]!) {
        at +=
          (current(e) * sweep(c.center, net.nodePos[e.a]!, net.nodePos[e.b]!)) /
          (2 * Math.PI);
      }
      coils[k]!.ampereTurns = Math.round(at * 1000) / 1000;
    });
    let dead = 0;
    for (const e of net.edges) {
      if (e.trace < 0 || pcb.traces[e.trace]!.net !== ph) continue;
      const ja = net.joinOf.get(e.a);
      if (ja !== undefined && ja === net.joinOf.get(e.b)) continue; // inside one pad/via
      if (Math.abs(current(e)) < 1e-3) dead += e.len;
    }
    const pr: PhaseReading = {
      net: ph,
      resistanceOhm: Math.round(V[pads[0]!]! * 1e4) / 1e4,
      deadCopperMm: Math.round(dead * 100) / 100,
    };
    if (dead > maxDead) {
      pr.problem = `${pr.deadCopperMm} mm of copper carries no current`;
      failures.push(
        `phase ${ph}: ${pr.problem} (dead-end stub — an open coil or a stranded branch)`,
      );
    }
    phases.push(pr);
  }

  // The absolute sense is a convention (flip every coil and the field just
  // rotates the other way against the same magnets); what must hold is that
  // every coil agrees with its plan polarity under ONE common sense.
  const net0 = coils.reduce((s, c) => s + c.ampereTurns * c.polarity, 0);
  const sense = net0 < 0 ? -1 : 1;
  for (const c of coils) {
    const signed = c.ampereTurns * c.polarity * sense;
    const fmt = `coil ${c.net} @ (${c.center.x}, ${c.center.y})`;
    if (Math.abs(c.ampereTurns) < 0.05 * spec.turns) {
      c.problem = "carries no current (open coil)";
    } else if (signed < 0) {
      c.problem =
        `current circulates against the rest of the winding (${c.ampereTurns} AT, ` +
        `plan polarity ${c.polarity})`;
    } else if (Math.abs(signed / spec.turns - 1) > tol) {
      c.problem = `${c.ampereTurns} AT per amp, expected ${spec.turns} (partly shorted or bypassed)`;
    } else {
      c.ok = true;
    }
    if (c.problem) failures.push(`${fmt}: ${c.problem}`);
  }

  return { pass: failures.length === 0, sense, coils, phases, failures };
}
