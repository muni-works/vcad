/**
 * The solid bodies a document holds, whichever way it holds them.
 *
 * A document carries geometry two ways: scene roots (`doc.roots`) and assembly
 * parts (`partDefs` + `instances`). The tools that pick one body to work on
 * (board_from_solid, check_enclosure_fit, analyze_structure, read) go through
 * here, so a document can hold a board, an enclosure and an assembly at once
 * and any of them can be addressed by name.
 *
 * Instance meshes are returned world-posed, because the callers work in
 * document coordinates (load regions, cavity frames, board outlines).
 */

import type { Document } from "@vcad/ir";
import { transformMesh, type EvaluatedScene, type Engine, type TriangleMesh } from "@vcad/engine";
import { partIdForNode } from "./part-ids.js";

export interface DocumentSolid {
  /** Scene-root node id, or the part def's root node for an instance. */
  rootId: number;
  /** Stable part id, or the instance id. */
  id: string;
  name?: string;
  /** Other names that select this body: its part def's id and name. */
  aliases?: string[];
  mesh: TriangleMesh;
}

/**
 * The node ids from a scene root down through its transform chain, stopping at
 * the first non-transform op. A board straight from `place_components` is a
 * bare PcbBoard root; any engine edit to the document rebuilds it as
 * Translate → Rotate → Scale → PcbBoard.
 */
export function transformChain(doc: Document, rootId: number): number[] {
  const chain = [rootId];
  for (let id = rootId; ; ) {
    const op = doc.nodes[String(id)]?.op as { type?: string; child?: number } | undefined;
    if (!op || !["Translate", "Rotate", "Scale"].includes(op.type ?? "") || op.child === undefined) {
      return chain;
    }
    id = op.child;
    chain.push(id);
  }
}

export function isBoardRoot(doc: Document, root: Document["roots"][number]): boolean {
  const leaf = transformChain(doc, root.root).at(-1)!;
  return (doc.nodes[String(leaf)]?.op as { type?: string } | undefined)?.type === "PcbBoard";
}

/**
 * The document with its PcbBoard scene roots removed. A board's kernel solid
 * carries every trace, via and pour (145k triangles and ~3 s on a routed
 * four-layer board), so a caller that discards the board must not evaluate it.
 */
export function withoutBoardRoots(doc: Document): Document {
  if (!doc.roots.some((r) => isBoardRoot(doc, r))) return doc;
  return { ...doc, roots: doc.roots.filter((r) => !isBoardRoot(doc, r)) };
}

/**
 * `engine.evaluate`, except PcbBoard roots go through their own minimal
 * document. The engine caches scenes by whole-document content, so any edit
 * anywhere re-tessellated the board; evaluated apart, the board's scene is
 * keyed by the board alone and an edit elsewhere is a cache hit. Parts come
 * back in visible-root order, exactly as `engine.evaluate` returns them.
 */
export function evaluateBoardsApart(doc: Document, engine: Engine): EvaluatedScene {
  const visible = doc.roots.filter((r) => r.visible !== false);
  const boardRoots = visible.filter((r) => isBoardRoot(doc, r));
  if (boardRoots.length === 0) return engine.evaluate(doc);

  const nodes: Document["nodes"] = {};
  for (const r of boardRoots) {
    for (const id of transformChain(doc, r.root)) nodes[String(id)] = doc.nodes[String(id)]!;
  }
  const boardDoc = {
    version: doc.version,
    nodes,
    materials: doc.materials,
    part_materials: doc.part_materials,
    roots: boardRoots,
  } as Document;
  const boards = engine.evaluate(boardDoc);
  const rest = engine.evaluate(withoutBoardRoots(doc));

  let b = 0;
  let o = 0;
  const parts = visible.flatMap((r) => {
    const part = isBoardRoot(doc, r) ? boards.parts[b++] : rest.parts[o++];
    return part ? [part] : [];
  });
  return { ...rest, parts };
}

const CHILD_FIELDS = ["child", "left", "right", "sketch", "parent"] as const;
const CHILD_LIST_FIELDS = ["sketches", "children"] as const;
const PIECE_CACHE_LIMIT = 512;
/** Hash of a root's canonical subgraph → its evaluated scene. Oldest out first. */
const pieceCache = new Map<string, EvaluatedScene>();

function isIdentityTransform(op: Document["nodes"][string]["op"]): boolean {
  const is = (v: { x: number; y: number; z: number }, n: number) => v.x === n && v.y === n && v.z === n;
  if (op.type === "Translate") return is(op.offset, 0);
  if (op.type === "Rotate") return is(op.angles, 0);
  if (op.type === "Scale") return is(op.factor, 1);
  return false;
}

/**
 * The nodes under `roots`, renumbered from zero in visit order. The engine
 * gives every node a fresh id each time it rebuilds a document, so the same
 * part hashes differently across edits unless its ids are normalized first.
 */
function canonicalNodes(doc: Document, roots: number[]) {
  const ids = new Map<number, number>();
  let nextId = 0;
  const nodes: Document["nodes"] = {};
  const bound = new Set(Object.keys(doc.bindings ?? {}).map((k) => Number(k.slice(0, k.indexOf(":")))));
  const visit = (id: number): number => {
    const known = ids.get(id);
    if (known !== undefined) return known;
    const node = doc.nodes[String(id)];
    if (!node) throw new Error(`dangling node ${id}`);
    // The engine wraps every feature in Translate/Rotate/Scale; an identity
    // wrapper is the same geometry, so a first edit needn't re-solve.
    if (!bound.has(id) && isIdentityTransform(node.op)) {
      const child = visit((node.op as unknown as { child: number }).child);
      ids.set(id, child);
      return child;
    }
    const canonical = nextId++;
    ids.set(id, canonical);
    const op = { ...node.op } as unknown as Record<string, unknown>;
    for (const f of CHILD_FIELDS) if (typeof op[f] === "number") op[f] = visit(op[f] as number);
    for (const f of CHILD_LIST_FIELDS) {
      if (Array.isArray(op[f])) op[f] = (op[f] as number[]).map(visit);
    }
    nodes[String(canonical)] = { id: canonical, name: null, op: op as unknown as typeof node.op };
    return canonical;
  };
  return { nodes, map: roots.map(visit), ids };
}

/**
 * JSON with object keys sorted, so the cache key is a function of content
 * only. The document engine re-serializes ops through serde (keys in field
 * order) while loon emits them in its own order: with a plain
 * `JSON.stringify` key, the first engine mutation after authoring missed the
 * cache for every untouched part and re-solved them all — a stator's
 * booleans turned the first `delete` into a 7–9 s call.
 */
export function stableKey(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value) ?? "null";
  if (Array.isArray(value)) return `[${value.map(stableKey).join(",")}]`;
  const entries = Object.entries(value as Record<string, unknown>)
    .filter(([, v]) => v !== undefined)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  return `{${entries.map(([k, v]) => `${JSON.stringify(k)}:${stableKey(v)}`).join(",")}}`;
}

/** Test seam: how many pieces were actually evaluated (cache misses). */
let pieceEvaluations = 0;
export function incrementalEvaluationCount(): number {
  return pieceEvaluations;
}

function evaluatePiece(piece: Document, engine: Engine): EvaluatedScene {
  const key = stableKey(piece);
  const hit = pieceCache.get(key);
  if (hit) return hit;
  pieceEvaluations++;
  const scene = engine.evaluate(piece);
  if (scene.failures?.length) return scene;
  pieceCache.set(key, scene);
  if (pieceCache.size > PIECE_CACHE_LIMIT) pieceCache.delete(pieceCache.keys().next().value!);
  return scene;
}

/**
 * `engine.evaluate` for a report that runs after every mutation: each root is
 * evaluated apart and remembered by content, so an edit re-solves only what it
 * changed. Whole-document evaluation is the fallback whenever a part fails, so
 * failures are reported exactly as before.
 */
export function evaluateIncrementally(doc: Document, engine: Engine): EvaluatedScene {
  const base = {
    version: doc.version,
    materials: {},
    part_materials: {},
    ...(doc.parameters ? { parameters: doc.parameters } : {}),
  };
  const bindingsFor = (ids: Map<number, number>) => {
    const out: Record<string, unknown> = {};
    for (const [key, expr] of Object.entries(doc.bindings ?? {})) {
      const at = key.indexOf(":");
      const id = ids.get(Number(key.slice(0, at)));
      if (id !== undefined) out[`${id}:${key.slice(at + 1)}`] = expr;
    }
    return Object.keys(out).length ? { bindings: out } : {};
  };
  try {
    const parts = doc.roots
      .filter((r) => r.visible !== false)
      .flatMap((root) => {
        const { nodes, map, ids } = canonicalNodes(doc, [root.root]);
        const piece = { ...base, nodes, roots: [{ root: map[0], material: "default" }], ...bindingsFor(ids) };
        const scene = evaluatePiece(piece as unknown as Document, engine);
        if (scene.failures?.length) throw new Error("part failed");
        return scene.parts.map((part) => ({ ...part, material: root.material }));
      });
    const defs = Object.entries(doc.partDefs ?? {}).sort(([a], [b]) => a.localeCompare(b));
    let instances: Partial<EvaluatedScene> = {};
    if (defs.length) {
      const { nodes, map, ids } = canonicalNodes(doc, defs.map(([, d]) => d.root));
      const partDefs = Object.fromEntries(defs.map(([k, d], i) => [k, { ...d, root: map[i] }]));
      const assembly = {
        ...base,
        materials: doc.materials,
        nodes,
        roots: [],
        partDefs,
        instances: doc.instances,
        joints: doc.joints,
        groundInstanceId: doc.groundInstanceId,
        ...bindingsFor(ids),
      };
      const scene = evaluatePiece(assembly as unknown as Document, engine);
      if (scene.failures?.length) throw new Error("part failed");
      instances = { partDefs: scene.partDefs, instances: scene.instances };
    }
    return { parts, clashes: [], ...instances } as EvaluatedScene;
  } catch {
    return evaluateBoardsApart(doc, engine);
  }
}

/** Test seam: forget remembered per-root evaluations. */
export function clearIncrementalEvaluation(): void {
  pieceCache.clear();
}

/** The assembly instances in an evaluated scene, posed in the world frame. */
export function instanceSolids(doc: Document, scene: EvaluatedScene): DocumentSolid[] {
  const out: DocumentSolid[] = [];
  for (const inst of scene.instances ?? []) {
    if (inst.mesh.positions.length === 0) continue;
    const def = doc.partDefs?.[inst.partDefId];
    out.push({
      rootId: def?.root ?? -1,
      id: inst.instanceId,
      name: inst.name ?? inst.instanceId,
      aliases: [inst.partDefId, ...(def?.name ? [def.name] : [])],
      mesh: inst.transform
        ? transformMesh(inst.mesh, {
            translate: inst.transform.translation,
            rotate: inst.transform.rotation,
            scale: inst.transform.scale,
          })
        : inst.mesh,
    });
  }
  return out;
}

/** Every non-empty solid in the document: scene roots (PCBs excluded), then instances. */
export function documentSolids(doc: Document, engine: Engine): DocumentSolid[] {
  // Boards are excluded below, so don't pay to tessellate them.
  const solids = withoutBoardRoots(doc);
  const scene = engine.evaluate(solids);
  const visibleRoots = solids.roots.filter((e) => e.visible !== false);
  const out: DocumentSolid[] = [];
  for (let i = 0; i < scene.parts.length && i < visibleRoots.length; i++) {
    const rootId = visibleRoots[i].root;
    const node = doc.nodes[String(rootId)];
    if ((node?.op as { type?: string } | undefined)?.type === "PcbBoard") continue;
    const mesh = scene.parts[i].mesh;
    if (!mesh || mesh.positions.length === 0) continue;
    out.push({ rootId, id: partIdForNode(doc, rootId), name: node?.name ?? undefined, mesh });
  }
  out.push(...instanceSolids(doc, scene));
  return out;
}

/** Assembly entries for `read`'s part list: instances, then uninstanced part defs. */
export function assemblyPartList(
  doc: Document,
): Array<{ id: string; name: string; kind: string; part_def: string }> {
  const kindOf = (defId: string) => {
    const root = doc.partDefs?.[defId]?.root;
    return (root !== undefined ? doc.nodes[String(root)]?.op?.type : undefined) ?? "Unknown";
  };
  const instances = doc.instances ?? [];
  const instanced = new Set(instances.map((i) => i.partDefId));
  return [
    ...instances.map((inst) => ({
      id: inst.id,
      name: inst.name ?? inst.id,
      kind: kindOf(inst.partDefId),
      part_def: inst.partDefId,
    })),
    ...Object.entries(doc.partDefs ?? {})
      .filter(([id]) => !instanced.has(id))
      .map(([id, def]) => ({ id, name: def.name ?? id, kind: kindOf(id), part_def: id })),
  ];
}

/** Find an assembly part by instance id/name or part def id/name. */
export function findAssemblyPart(
  doc: Document,
  wanted: string,
): { id: string; root: number; material?: string } | null {
  const defs = doc.partDefs ?? {};
  const inst = (doc.instances ?? []).find((i) => i.id === wanted || i.name === wanted);
  if (inst && defs[inst.partDefId]) {
    const def = defs[inst.partDefId]!;
    return { id: inst.id, root: def.root, material: inst.material ?? def.defaultMaterial ?? undefined };
  }
  const entry = Object.entries(defs).find(([id, d]) => id === wanted || d.name === wanted);
  if (!entry) return null;
  return { id: entry[0], root: entry[1].root, material: entry[1].defaultMaterial ?? undefined };
}
