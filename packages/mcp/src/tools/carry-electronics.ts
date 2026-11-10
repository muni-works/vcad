/**
 * Keeping a session's electronics across a loon re-author. Lives apart from
 * the create_cad_loon tool because it is pure document surgery, reachable from
 * the browser tool set, while that tool reads the filesystem.
 */

import type { Document } from "@vcad/ir";
import { getPcbNodeIds } from "@vcad/core";

type EcadDoc = Document & { pcb?: unknown; __pendingDesignRules?: unknown };

/**
 * Carry a session's electronics onto a freshly evaluated loon document, in
 * place: loon authors mechanical geometry, so re-authoring must not take the
 * board and schematic with it. Board nodes move to fresh ids (loon numbers its
 * nodes from zero, so they would collide) along with their bindings and any
 * constraint anchored purely on the board.
 *
 * Throws, before anything is written, when the loon program brings its own
 * board or schematic: two boards can't be merged and neither should silently
 * win. Returns a note for the caller, or null when there was nothing to carry.
 */
export function carryElectronics(session: Document, loonDoc: Document): string | null {
  const from = session as EcadDoc;
  const to = loonDoc as EcadDoc;
  const boardIds = getPcbNodeIds(session);
  if (!boardIds.length && !from.pcb && !from.schematic && !from.__pendingDesignRules) return null;
  if (
    ((boardIds.length || from.pcb) && (getPcbNodeIds(loonDoc).length || to.pcb)) ||
    (from.schematic && to.schematic)
  ) {
    throw new Error(
      "create_cad_loon: this session already holds a board or schematic and the loon program " +
        "defines its own, so they can't be merged — nothing was changed. Remove them from the " +
        "loon source, or omit document_id to author into a new session.",
    );
  }

  // Once the engine has materialized a document, a board's scene root is not
  // the PcbBoard node but the Translate above it. Carry that whole chain:
  // keyed only on the board node, the root stayed behind and the board went
  // unrooted, so the next engine load (the app window, any `add_mesh_part`)
  // dropped it.
  const chains = session.roots.flatMap((entry) => {
    const chain = boardChain(session, entry.root);
    return chain ? [{ entry, chain }] : [];
  });
  const rooted = new Set(chains.map(({ chain }) => chain[chain.length - 1]!));
  const carried = [...new Set([...chains.flatMap(({ chain }) => chain), ...boardIds])];

  let next = Math.max(-1, ...Object.keys(to.nodes).map(Number)) + 1;
  const remap = new Map<number, number>(carried.map((oldId) => [oldId, next++]));
  const keepMaterial = (key: string | undefined) => {
    if (key && session.materials[key]) to.materials[key] ??= session.materials[key]!;
  };
  for (const { entry, chain } of chains) {
    to.roots.push({ ...entry, root: remap.get(chain[0]!)! });
    keepMaterial(entry.material);
  }
  for (const oldId of boardIds) {
    if (rooted.has(oldId)) continue;
    to.roots.push({ root: remap.get(oldId)!, material: "__pcb_fr4__" });
    keepMaterial("__pcb_fr4__");
  }
  for (const oldId of carried) {
    const id = remap.get(oldId)!;
    const node = session.nodes[String(oldId)]!;
    const op = node.op as { child?: number };
    to.nodes[String(id)] = {
      ...node,
      id,
      ...(typeof op.child === "number"
        ? { op: { ...node.op, child: remap.get(op.child)! } as typeof node.op }
        : {}),
    };
    const stableId = session.partIds?.[String(oldId)];
    if (stableId) to.partIds = { ...to.partIds, [String(id)]: stableId };
    for (const [key, expr] of Object.entries(session.bindings ?? {})) {
      if (!key.startsWith(`${oldId}:`)) continue;
      to.bindings = { ...to.bindings, [`${id}:${key.slice(`${oldId}:`.length)}`]: expr };
      for (const name of (typeof expr === "string" ? expr.match(/[A-Za-z_]\w*/g) : null) ?? []) {
        const param = session.parameters?.[name];
        if (param && !to.parameters?.[name]) to.parameters = { ...to.parameters, [name]: param };
      }
    }
  }

  // A constraint survives only if everything it anchors to survived.
  let droppedConstraints = 0;
  for (const c of session.constraints ?? []) {
    const clone = JSON.parse(JSON.stringify(c)) as unknown;
    let onBoardOnly = true;
    const visit = (v: unknown): void => {
      if (Array.isArray(v)) return v.forEach(visit);
      if (!v || typeof v !== "object") return;
      const o = v as Record<string, unknown>;
      if (typeof o.kind === "string" && "node" in o) {
        const moved = o.kind.startsWith("pcb") ? remap.get(o.node as number) : undefined;
        if (moved === undefined) onBoardOnly = false;
        else o.node = moved;
      }
      Object.values(o).forEach(visit);
    };
    visit(clone);
    if (onBoardOnly && remap.size) (to.constraints ??= []).push(clone as (typeof c));
    else droppedConstraints++;
  }

  if (from.pcb) to.pcb = from.pcb;
  if (from.schematic) to.schematic = from.schematic;
  if (from.__pendingDesignRules) to.__pendingDesignRules = from.__pendingDesignRules;

  const kept = [
    boardIds.length || from.pcb ? "PCB board" : null,
    from.schematic ? "schematic" : null,
  ].filter(Boolean);
  return (
    `Mechanical geometry replaced; kept the session's ${kept.join(" and ") || "pending design rules"}.` +
    (droppedConstraints
      ? ` Dropped ${droppedConstraints} constraint(s) anchored on the replaced geometry.`
      : "")
  );
}

/** The transform wrappers from `root` down to a PcbBoard, board last; null if it isn't one. */
function boardChain(doc: Document, root: number): number[] | null {
  const chain: number[] = [];
  for (let id: number | undefined = root; id !== undefined; ) {
    const op = doc.nodes[String(id)]?.op as { type: string; child?: number } | undefined;
    if (!op || chain.includes(id)) return null;
    chain.push(id);
    if (op.type === "PcbBoard") return chain;
    id = op.type === "Translate" || op.type === "Rotate" || op.type === "Scale" ? op.child : undefined;
  }
  return null;
}
