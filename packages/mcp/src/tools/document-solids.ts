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
  const scene = engine.evaluate(doc);
  const visibleRoots = doc.roots.filter((e) => e.visible !== false);
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
