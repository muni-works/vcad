/**
 * Closed-form correction of tessellated part-pair clearances.
 *
 * `engine.meshClearance` measures polygons: a cylinder meshed at n segments
 * has chords up to r·(1 − cos(π/n)) inside its true surface, so a running fit
 * tighter than that chord sag (a 7.98 mm-radius shaft in an 8.00 mm bore)
 * can read as an interference. The kernel's `refineClearance` recognises
 * when a pair's mesh witness sits on two parallel cylinders and returns the
 * exact gap instead — the same rule `Solid::clearance` applies natively
 * (`vcad_kernel::analytic_clearance`), so the app's live receipt badges and
 * this server agree.
 *
 * Best-effort and fail-closed: when the kernel build lacks the export, a part
 * isn't a BRep document root (assembly instances, mesh imports), or the
 * closed form doesn't provably explain the mesh number, the mesh result
 * stands unchanged.
 */

import type { Document } from "@vcad/ir";
import type { ClearanceResult, Engine } from "@vcad/engine";
import type { PoseablePart } from "./sweep.js";

/** Only pairs within this of contact (or intersecting) are refined: one
 *  document re-evaluation per batch is not worth paying for far pairs,
 *  whose chord error is irrelevant to any verdict. */
export const REFINE_REACH_MM = 1.0;

/** A clearance result, possibly corrected by the closed form. */
export type MeasuredClearance = ClearanceResult & {
  /** Set once refinement ran: `"analytic_cylinders"` when it replaced the mesh number. */
  basis?: "mesh" | "analytic_cylinders";
  /** Mesh distance before the correction, mm (analytic only). */
  meshDistance?: number;
  /** Combined chord-sag band of the cylinder pair, mm (analytic only). */
  chordBandMm?: number;
};

/** Ids of parts whose mesh is in the document's BRep frame (static roots). */
export function staticPartIds(base: PoseablePart[]): Set<string> {
  return new Set(base.filter((p) => !p.instanceId && !p.transform).map((p) => p.id));
}

/**
 * Refine `items` in place. Each item names its two part ids and carries the
 * mesh result measured between them (in that order). Pairs not in
 * `refinable`, already refined, or farther than {@link REFINE_REACH_MM}
 * are skipped.
 */
export function refineClearances(
  doc: Document,
  engine: Engine,
  items: Array<{ a: string; b: string; r: MeasuredClearance }>,
  refinable: Set<string>,
): void {
  if (typeof engine.hasRefineClearance !== "function" || !engine.hasRefineClearance()) return;
  const todo = items.filter(
    (x) =>
      x.r.basis === undefined &&
      refinable.has(x.a) &&
      refinable.has(x.b) &&
      (x.r.intersecting || x.r.distance < REFINE_REACH_MM),
  );
  if (todo.length === 0) return;
  let out: ReturnType<Engine["refineClearance"]>;
  try {
    out = engine.refineClearance(
      JSON.stringify(doc),
      todo.map((x) => ({ a: x.a, b: x.b, mesh: x.r })),
    );
  } catch {
    // Refinement is an accuracy upgrade, never a gate: the mesh number stands.
    return;
  }
  todo.forEach((x, i) => {
    const o = out[i];
    x.r.basis = o?.basis ?? "mesh";
    if (o && o.basis === "analytic_cylinders") {
      x.r.distance = o.distance;
      x.r.intersecting = o.intersecting;
      x.r.pointA = o.pointA;
      x.r.pointB = o.pointB;
      x.r.meshDistance = o.meshDistance;
      if (o.chordBandMm !== undefined) x.r.chordBandMm = o.chordBandMm;
    }
  });
}
