/**
 * `check_enclosure_fit` — cross-domain PCB ↔ enclosure verification.
 *
 * The differentiator no EDA tool can match: vcad holds a real BRep CAD kernel
 * *and* a PCB engine in one session store, so it can co-verify a board against
 * the physical case it ships in. This tool takes a board session and a CAD
 * session holding the enclosure solid, extracts the case cavity (floor, walls,
 * standoffs, wall cutouts) from its mesh, and runs the four
 * {@link checkEnclosureFit} checks: board fits with clearance, components clear
 * the lid, mounting holes land on standoffs, connectors line up with cutouts.
 *
 * The verdict is also surfaced through `build_receipt` (attach an
 * `enclosure_document_id`) so a board's durable proof can include "and it fits
 * its case."
 */

import type { Document, Pcb } from "@vcad/ir";
import { getNodePcb, getPcbNodeIds } from "@vcad/core";
import {
  checkEnclosureFit as computeEnclosureFit,
  componentMeshes,
  componentExtentsFromMeshes,
  connectorsFromPcb,
  deriveBoardFromCavity,
  extractEnclosureFeatures,
  mountingHolesFromPcb,
  type BoardPlacement,
  type ComponentExtent,
  type Engine,
  type EnclosureFitReport,
  type TriangleMesh,
} from "@vcad/engine";
import { getSession } from "./session.js";
import { findPartCandidate } from "./part-ids.js";
import { documentSolids } from "./document-solids.js";
import { sealedCavityFromMesh } from "./ecad.js";
import { behavior, type ToolDef } from "./tool-def.js";

/** PCB data from a document — PcbBoard nodes first, legacy `doc.pcb` fallback. */
function getDocPcb(doc: Document): Pcb | null {
  const nodeIds = getPcbNodeIds(doc);
  if (nodeIds.length > 0) return getNodePcb(doc, nodeIds[0]!);
  return (doc as Document & { pcb?: Pcb }).pcb ?? null;
}

const vec3Schema = {
  type: "object" as const,
  properties: {
    x: { type: "number" as const },
    y: { type: "number" as const },
    z: { type: "number" as const },
  },
  required: ["x", "y", "z"],
};

export const checkEnclosureFitSchema = {
  type: "object" as const,
  properties: {
    document_id: {
      type: "string" as const,
      description: "Board session id (from create_schematic / place_components) holding the PCB to verify.",
    },
    enclosure_document_id: {
      type: "string" as const,
      description:
        "CAD session id (from open_document / create_cad_loon) holding the enclosure solid the board lives in.",
    },
    enclosure_part_id: {
      type: "string" as const,
      description:
        "Part id, name, assembly instance, or part def of the enclosure when the CAD session has more than one solid.",
    },
    clearance: {
      type: "number" as const,
      description: "All-round clearance the board needs from the cavity walls and lid (mm). Default 0.5.",
    },
    standoff_height: {
      type: "number" as const,
      description: "Board lift above the cavity floor when the case has no detectable standoffs (mm). Default 0.",
    },
    board_offset: {
      ...vec3Schema,
      description:
        "Where the board's local origin sits in the enclosure-world frame. Omit to auto-fit (center the board on the standoffs).",
    },
    board_rotation_deg: {
      type: "number" as const,
      description: "Board rotation about Z in the enclosure frame (degrees). Default 0.",
    },
    hole_tolerance: {
      type: "number" as const,
      description: "Mounting-hole to standoff alignment tolerance (mm). Default 0.6.",
    },
    derive: {
      type: "boolean" as const,
      description:
        "Also return a board outline + mounting holes auto-derived from the cavity (the co-design starting point).",
    },
  },
  required: ["document_id", "enclosure_document_id"],
} as const;

type EnclosureCandidate = { mesh: TriangleMesh; rootId: number; id: string; name?: string };

/**
 * Evaluate a CAD session and return the enclosure candidates in preference
 * order (skips PcbBoard). An explicit `partId` yields exactly that part.
 * Otherwise every solid is a candidate, biggest bounding box first — the
 * caller takes the first one that actually has a cavity, so a dense stator or
 * motor sharing the document can't shadow the case it sits in.
 */
function enclosureCandidates(
  enclosureDoc: Document,
  engine: Engine,
  partId?: string,
): EnclosureCandidate[] {
  const candidates = documentSolids(enclosureDoc, engine);
  if (candidates.length === 0) {
    throw new Error("Enclosure document has no solid parts (PcbBoard parts are excluded)");
  }
  if (partId) {
    const found = findPartCandidate(enclosureDoc, candidates, partId);
    if (!found) {
      throw new Error(
        `No enclosure part with id "${partId}". Available: ${candidates
          .map((c) => `${c.id}${c.name ? ` (${c.name})` : ""}`)
          .join(", ")}`,
      );
    }
    return [found];
  }
  const bboxVolume = (m: TriangleMesh) => {
    const p = m.positions;
    const lo = [Infinity, Infinity, Infinity];
    const hi = [-Infinity, -Infinity, -Infinity];
    for (let i = 0; i + 2 < p.length; i += 3) {
      for (let k = 0; k < 3; k++) {
        lo[k] = Math.min(lo[k], p[i + k]);
        hi[k] = Math.max(hi[k], p[i + k]);
      }
    }
    return (hi[0] - lo[0]) * (hi[1] - lo[1]) * (hi[2] - lo[2]);
  };
  return candidates
    .map((c) => ({ c, v: bboxVolume(c.mesh) }))
    .sort((a, b) => b.v - a.v)
    .map(({ c }) => c);
}

/** Result of the shared computation, reused by the tool and build_receipt. */
export interface EnclosureFitResult {
  report?: EnclosureFitReport;
  derived?: {
    outline: ReturnType<typeof deriveBoardFromCavity>["outline"];
    mountingHoles: ReturnType<typeof deriveBoardFromCavity>["mountingHoles"];
    placement: BoardPlacement;
  };
  cavity?: ReturnType<typeof extractEnclosureFeatures>["cavity"];
  standoffs_detected?: number;
  openings_detected?: number;
  cavity_basis?: "sealed-void";
  note?: string;
  error?: string;
}

type ComponentBody = { footprint_ref: string; positions: ArrayLike<number> };

/**
 * The board-fit check measures the outline. Parts hang past it, connectors most
 * of all, and a body through a solid wall is a collision whatever the outline
 * says: `check_clearance` reports it as penetration. A wall with openings may
 * be letting the connector through, so that only warns; the alignment check
 * has the detail.
 */
export function addComponentOverhangCheck(
  report: EnclosureFitReport,
  bodies: ComponentBody[],
  cavity: { minX: number; maxX: number; minY: number; maxY: number },
  hasOpenings: boolean,
): void {
  const t = (report.placement.rotationDeg * Math.PI) / 180;
  const [sin, cos] = [Math.sin(t), Math.cos(t)];
  const { x: ox, y: oy } = report.placement.offset;
  const over = bodies.flatMap((b) => {
    let worst = 0;
    for (let i = 0; i + 2 < b.positions.length; i += 3) {
      const [x, y] = [b.positions[i]!, b.positions[i + 1]!];
      const wx = ox + x * cos - y * sin;
      const wy = oy + x * sin + y * cos;
      worst = Math.max(worst, cavity.minX - wx, wx - cavity.maxX, cavity.minY - wy, wy - cavity.maxY);
    }
    return worst > 1e-3 ? [{ ref: b.footprint_ref, mm: Math.round(worst * 100) / 100 }] : [];
  });
  if (bodies.length === 0) return;
  const worst = over.reduce((m, o) => Math.max(m, o.mm), 0);
  const names = over.map((o) => `${o.ref} (${o.mm} mm)`).join(", ");
  report.checks.push({
    id: "component_overhang",
    label: "Component bodies inside cavity",
    status: over.length === 0 ? "pass" : hasOpenings ? "warn" : "fail",
    detail:
      over.length === 0
        ? `All ${bodies.length} component bodies stay inside the cavity`
        : hasOpenings
          ? `Past the cavity wall: ${names}. Confirm each passes through a wall opening.`
          : `Through a solid wall, and the case has no openings: ${names}. check_clearance reports this as penetration.`,
    measurements: { overhanging: over.length, worst_mm: worst },
  });
  if (over.length === 0) return;
  report.verified = false;
  if (hasOpenings) return;
  report.ok = false;
  report.summary = `Component bodies pass through the case wall: ${names}. ${report.summary}`;
}

/** Grid pitch for scanning a wall (mm); coarsened to stay within the cap. */
const WALL_SCAN_CELL = 0.25;
const WALL_SCAN_MAX_CELLS = 512;
/** A gap narrower or shorter than this is a mesh artifact, not a cutout (mm). */
const MIN_OPENING_MM = 1;

type WallOpenings = ReturnType<typeof extractEnclosureFeatures>["openings"];
type SealedCavity = NonNullable<ReturnType<typeof sealedCavityFromMesh>>;

/**
 * Cutouts through the walls of a closed case. Looking outward from each cavity
 * wall, a patch of wall with no surface at or beyond it is open to the outside:
 * a blind pocket still has the outer skin behind it. Every triangle reaching
 * the wall is projected onto it and rasterized; what stays uncovered, grouped
 * into connected regions, is an opening.
 */
export function sealedWallOpenings(mesh: TriangleMesh, cavity: SealedCavity): WallOpenings {
  const walls = [
    { edge: "minX", normal: 0, sign: -1, at: cavity.minX, from: cavity.minY, to: cavity.maxY },
    { edge: "maxX", normal: 0, sign: 1, at: cavity.maxX, from: cavity.minY, to: cavity.maxY },
    { edge: "minY", normal: 1, sign: -1, at: cavity.minY, from: cavity.minX, to: cavity.maxX },
    { edge: "maxY", normal: 1, sign: 1, at: cavity.maxY, from: cavity.minX, to: cavity.maxX },
  ] as const;
  const { positions: pos, indices: idx } = mesh;
  const openings: WallOpenings = [];

  for (const w of walls) {
    const tangent = 1 - w.normal;
    const [spanU, spanZ] = [w.to - w.from, cavity.ceilZ - cavity.floorZ];
    if (spanU <= 0 || spanZ <= 0) continue;
    const cell = Math.max(WALL_SCAN_CELL, Math.max(spanU, spanZ) / WALL_SCAN_MAX_CELLS);
    const nu = Math.ceil(spanU / cell);
    const nz = Math.ceil(spanZ / cell);
    const [du, dz] = [spanU / nu, spanZ / nz];
    const covered = new Uint8Array(nu * nz);

    for (let t = 0; t + 2 < idx.length; t += 3) {
      const [a, b, c] = [idx[t]! * 3, idx[t + 1]! * 3, idx[t + 2]! * 3];
      const [ad, bd, cd] = [pos[a + w.normal]!, pos[b + w.normal]!, pos[c + w.normal]!];
      if (Math.max(w.sign * (ad - w.at), w.sign * (bd - w.at), w.sign * (cd - w.at)) < -1e-3) continue;
      const [au, bu, cu] = [pos[a + tangent]!, pos[b + tangent]!, pos[c + tangent]!];
      const [az, bz, cz] = [pos[a + 2]!, pos[b + 2]!, pos[c + 2]!];
      const det = (bz - cz) * (au - cu) + (cu - bu) * (az - cz);
      if (Math.abs(det) < 1e-12) continue; // edge-on: a slot's side walls
      const i0 = Math.max(0, Math.floor((Math.min(au, bu, cu) - w.from) / du - 0.5));
      const i1 = Math.min(nu - 1, Math.ceil((Math.max(au, bu, cu) - w.from) / du - 0.5));
      const k0 = Math.max(0, Math.floor((Math.min(az, bz, cz) - cavity.floorZ) / dz - 0.5));
      const k1 = Math.min(nz - 1, Math.ceil((Math.max(az, bz, cz) - cavity.floorZ) / dz - 0.5));
      for (let k = k0; k <= k1; k++) {
        const z = cavity.floorZ + (k + 0.5) * dz;
        for (let i = i0; i <= i1; i++) {
          const u = w.from + (i + 0.5) * du;
          const l0 = ((bz - cz) * (u - cu) + (cu - bu) * (z - cz)) / det;
          const l1 = ((cz - az) * (u - cu) + (au - cu) * (z - cz)) / det;
          const l2 = 1 - l0 - l1;
          if (l0 < -1e-9 || l1 < -1e-9 || l2 < -1e-9) continue;
          if (w.sign * (l0 * ad + l1 * bd + l2 * cd - w.at) >= -1e-3) covered[k * nu + i] = 1;
        }
      }
    }

    const seen = new Uint8Array(nu * nz);
    for (let start = 0; start < covered.length; start++) {
      if (covered[start] || seen[start]) continue;
      seen[start] = 1;
      const stack = [start];
      let [iMin, iMax, kMin, kMax] = [nu, -1, nz, -1];
      while (stack.length) {
        const c = stack.pop()!;
        const [k, i] = [Math.floor(c / nu), c % nu];
        [iMin, iMax, kMin, kMax] = [Math.min(iMin, i), Math.max(iMax, i), Math.min(kMin, k), Math.max(kMax, k)];
        const next = [i > 0 ? c - 1 : -1, i < nu - 1 ? c + 1 : -1, k > 0 ? c - nu : -1, k < nz - 1 ? c + nu : -1];
        for (const n of next) {
          if (n < 0 || covered[n] || seen[n]) continue;
          seen[n] = 1;
          stack.push(n);
        }
      }
      const width = (iMax - iMin + 1) * du;
      if (width < MIN_OPENING_MM || (kMax - kMin + 1) * dz < MIN_OPENING_MM) continue;
      const along = w.from + ((iMin + iMax + 1) / 2) * du;
      openings.push({
        edge: w.edge,
        center: w.normal === 0 ? { x: w.at, y: along } : { x: along, y: w.at },
        width,
        zMin: cavity.floorZ + kMin * dz,
        zMax: cavity.floorZ + (kMax + 1) * dz,
      });
    }
  }
  return openings;
}

function footprint(c: { minX: number; maxX: number; minY: number; maxY: number }): number {
  return (c.maxX - c.minX) * (c.maxY - c.minY);
}

/**
 * The cavity a mesh encloses, or null for a solid block. The kernel detector
 * only knows open-top trays. A closed case has its cavity found the way
 * board_from_solid finds it and its wall cutouts scanned directly; posts
 * aren't extracted from a sealed void, and the result says so. That detector
 * also reads a through-slot in a closed lid as a shallow open pocket, so the
 * void wins whenever it is the bigger of the two.
 */
function cavityOf(mesh: TriangleMesh) {
  const features = extractEnclosureFeatures(mesh.positions, mesh.indices);
  const sealedVoid = sealedCavityFromMesh(mesh);
  const sealed =
    sealedVoid && (!features.cavity || footprint(sealedVoid) > footprint(features.cavity))
      ? sealedVoid
      : null;
  const cavity = sealed ?? features.cavity;
  if (!cavity) return null;
  return {
    cavity,
    sealed,
    standoffs: sealed ? [] : features.standoffs,
    openings: sealed ? sealedWallOpenings(mesh, sealed) : features.openings,
  };
}

/**
 * The fit options both `check_enclosure_fit` and `build_receipt` read from
 * their args — one parser, so the two tools pick and place the enclosure
 * identically.
 */
export function enclosureFitOptsFromArgs(args: Record<string, unknown>) {
  return {
    enclosurePartId: args.enclosure_part_id ? String(args.enclosure_part_id) : undefined,
    clearance: typeof args.clearance === "number" ? args.clearance : undefined,
    standoffHeight: typeof args.standoff_height === "number" ? args.standoff_height : undefined,
    holeTolerance: typeof args.hole_tolerance === "number" ? args.hole_tolerance : undefined,
    placement: placementFromArgs(args),
  };
}

/**
 * Core: given a board PCB and an enclosure CAD session, extract the cavity and
 * run the cross-domain checks. Pure of MCP plumbing so `check_enclosure_fit`
 * and `build_receipt` share one implementation.
 */
export async function computeEnclosureFitForBoard(
  pcb: Pcb,
  enclosureDoc: Document,
  engine: Engine,
  opts: {
    enclosurePartId?: string;
    clearance?: number;
    standoffHeight?: number;
    holeTolerance?: number;
    placement?: BoardPlacement;
    derive?: boolean;
  } = {},
): Promise<EnclosureFitResult> {
  let candidates: EnclosureCandidate[];
  try {
    candidates = enclosureCandidates(enclosureDoc, engine, opts.enclosurePartId);
  } catch (e) {
    return { error: e instanceof Error ? e.message : String(e) };
  }

  let found: ReturnType<typeof cavityOf> = null;
  for (const c of candidates) {
    found = cavityOf(c.mesh);
    if (found) break;
  }
  if (!found) {
    return {
      error:
        "No interior cavity found in the enclosure solid — it reads as a solid block. " +
        "check_enclosure_fit needs an open or closed case with a pocket the board sits in.",
    };
  }

  // Component Z extents from the kernel's 3D component bodies (best-effort: an
  // empty list degrades the lid check to a skip, never an error).
  let componentExtents: ComponentExtent[] = [];
  let bodies: ComponentBody[] = [];
  try {
    const meshes = await componentMeshes(pcb);
    if (meshes.length > 0) componentExtents = componentExtentsFromMeshes(meshes, pcb);
    bodies = meshes;
  } catch {
    /* WASM unavailable — lid clearance check will report skip */
  }

  const { cavity, sealed, standoffs, openings } = found;
  const mountingHoles = mountingHolesFromPcb(pcb);
  const connectors = connectorsFromPcb(pcb, pcb.outline);

  const report = computeEnclosureFit({
    outline: pcb.outline,
    cavity,
    standoffs: standoffs,
    openings: openings,
    mountingHoles,
    connectors,
    componentExtents,
    placement: opts.placement,
    clearance: opts.clearance,
    standoffHeight: opts.standoffHeight,
    holeTolerance: opts.holeTolerance,
  });
  addComponentOverhangCheck(report, bodies, cavity, openings.length > 0);

  const result: EnclosureFitResult = {
    report,
    cavity,
    standoffs_detected: standoffs.length,
    openings_detected: openings.length,
    ...(sealed
      ? {
          cavity_basis: "sealed-void" as const,
          note:
            "Closed case: the cavity is the largest box inside the sealed void, and wall openings " +
            "are scanned from it. Standoffs are not detected in a sealed case, so the mounting " +
            "check is not evidence of a fit — verify mounting separately.",
        }
      : {}),
  };
  if (opts.derive) {
    result.derived = deriveBoardFromCavity(cavity, standoffs, {
      clearance: opts.clearance,
      thickness: pcb.outline.thickness,
      standoffHeight: opts.standoffHeight,
    });
  }
  return result;
}

/** Read a board placement from tool args (offset + rotation), if present. */
function placementFromArgs(args: Record<string, unknown>): BoardPlacement | undefined {
  const off = args.board_offset as { x?: number; y?: number; z?: number } | undefined;
  if (!off || typeof off.x !== "number" || typeof off.y !== "number" || typeof off.z !== "number") {
    return undefined;
  }
  return {
    offset: { x: off.x, y: off.y, z: off.z },
    rotationDeg: typeof args.board_rotation_deg === "number" ? args.board_rotation_deg : 0,
  };
}

/** `check_enclosure_fit` MCP handler. */
export async function checkEnclosureFit(args: Record<string, unknown>, engine: Engine) {
  const boardId = String(args.document_id ?? "");
  const enclosureId = String(args.enclosure_document_id ?? "");
  if (!boardId) return err("Pass `document_id` (the board session).");
  if (!enclosureId) return err("Pass `enclosure_document_id` (the CAD session with the enclosure solid).");

  const boardDoc = getSession(boardId);
  const pcb = getDocPcb(boardDoc);
  if (!pcb) return err("Board document has no PCB — run place_components first.");
  const enclosureDoc = getSession(enclosureId);

  const res = await computeEnclosureFitForBoard(pcb, enclosureDoc, engine, {
    ...enclosureFitOptsFromArgs(args),
    derive: args.derive === true,
  });

  if (res.error || !res.report) return err(res.error ?? "Enclosure fit could not be computed.");

  const payload = {
    success: true,
    document_id: boardId,
    enclosure_document_id: enclosureId,
    ok: res.report.ok,
    verified: res.report.verified,
    summary: res.report.summary,
    clearance_mm: res.report.clearance,
    placement: res.report.placement,
    cavity: res.cavity,
    standoffs_detected: res.standoffs_detected,
    openings_detected: res.openings_detected,
    ...(res.cavity_basis ? { cavity_basis: res.cavity_basis, note: res.note } : {}),
    checks: res.report.checks,
    ...(res.derived ? { derived_board: res.derived } : {}),
  };

  return {
    content: [{ type: "text" as const, text: JSON.stringify(payload) }],
    structuredContent: {
      enclosure_fit: res.report,
      ...(res.derived ? { derived_board: res.derived } : {}),
      document_id: boardId,
      enclosure_document_id: enclosureId,
    },
  };
}

function err(text: string) {
  return {
    content: [{ type: "text" as const, text: `Error: ${text}` }],
    isError: true as const,
  };
}

export const toolDefs: ToolDef[] = [
  {
    name: "check_enclosure_fit",
    pack: "ecad",
    description:
      "Cross-check a board (board session) against the enclosure it ships " +
      "in (a CAD session holding the case solid) — the verification axis no " +
      "EDA tool has, because vcad owns both a BRep kernel and a PCB engine. " +
      "Extracts the case cavity, standoffs, and wall cutouts from the solid " +
      "mesh, then verifies: board fits with clearance, tall parts clear the " +
      "lid, mounting holes land on standoffs, and connectors line up with " +
      "the wall openings. Pass `derive:true` to also get a board outline + " +
      "holes seeded from the cavity. Surfaced in build_receipt too.",
    inputSchema: checkEnclosureFitSchema,
    handler: (a, c) => checkEnclosureFit(a, c.engine),
    behavior: behavior({}),
  },
];
