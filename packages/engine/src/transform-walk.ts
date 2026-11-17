/**
 * Walking a scene root through its Translate/Rotate/Scale wrapper chain.
 *
 * Several root kinds bypass the CSG `Solid` pipeline and are detected by
 * walking down from a scene root to the first node of interest, accumulating
 * the placement transform on the way: imported meshes, embroidery patterns,
 * and sheet-metal chains. {@link findWrappedRoot} is the one walker they all
 * share; {@link transformMesh}'s caller then applies the placement to the
 * mesh those finders produce.
 *
 * This lives in its own leaf module (no engine imports) so both `evaluate.ts`
 * and `sheet-metal.ts` can use it without an import cycle.
 */

import type { CsgOp, Node, NodeId } from "@vcad/ir";

/** A rigid placement read off a wrapper chain: the standard
 *  translate/rotate/scale triple {@link transformMesh} consumes.
 *
 *  `rotate` is in the PLACEMENT convention (R = Rz·Ry·Rx, X acts first —
 *  the instance `Transform3D` / three.js "ZYX" order), not the raw
 *  `CsgOp::Rotate` angles: {@link findWrappedRoot} converts. */
export interface TransformInfo {
  translate: { x: number; y: number; z: number };
  rotate: { x: number; y: number; z: number };
  scale: { x: number; y: number; z: number };
}

/** The neutral placement: no translation, no rotation, unit scale. */
export function identityTransform(): TransformInfo {
  return {
    translate: { x: 0, y: 0, z: 0 },
    rotate: { x: 0, y: 0, z: 0 },
    scale: { x: 1, y: 1, z: 1 },
  };
}

/** True when `t` is the identity placement — lets callers skip a mesh copy. */
export function isIdentityTransform(t: TransformInfo): boolean {
  return (
    t.translate.x === 0 &&
    t.translate.y === 0 &&
    t.translate.z === 0 &&
    t.rotate.x === 0 &&
    t.rotate.y === 0 &&
    t.rotate.z === 0 &&
    t.scale.x === 1 &&
    t.scale.y === 1 &&
    t.scale.z === 1
  );
}

/**
 * Convert `CsgOp::Rotate` angles (degrees) to the placement-convention
 * angles {@link transformMesh} expects, describing the same rotation.
 *
 * `CsgOp::Rotate` is R = Rx·Ry·Rz (Z acts first): that is what the kernel's
 * `Solid.rotate` does to a BRep, and the evaluator applies it to imported
 * meshes too. {@link transformMesh} is R = Rz·Ry·Rx (X first) because it also
 * places instances (`Transform3D`). The two agree when at most one angle is
 * nonzero, so that case passes through untouched (bit-exact); otherwise go
 * through the matrix.
 */
export function rotateOpToPlacementEuler(a: {
  x: number;
  y: number;
  z: number;
}): { x: number; y: number; z: number } {
  const nonzero = (a.x !== 0 ? 1 : 0) + (a.y !== 0 ? 1 : 0) + (a.z !== 0 ? 1 : 0);
  if (nonzero <= 1) return { x: a.x, y: a.y, z: a.z };
  const d = Math.PI / 180;
  const [sa, ca] = [Math.sin(a.x * d), Math.cos(a.x * d)];
  const [sb, cb] = [Math.sin(a.y * d), Math.cos(a.y * d)];
  const [sc, cc] = [Math.sin(a.z * d), Math.cos(a.z * d)];
  // M = Rx(a)·Ry(b)·Rz(c), row-major (only the entries the decomposition reads).
  const m00 = cb * cc;
  const m10 = ca * sc + sa * sb * cc;
  const m11 = ca * cc - sa * sb * sc;
  const m12 = -sa * cb;
  const m20 = sa * sc - ca * sb * cc;
  const m21 = sa * cc + ca * sb * sc;
  const m22 = ca * cb;
  // Decompose as Rz(z)·Ry(y)·Rx(x): row 2 = [-sy, sx·cy, cx·cy].
  const sy = Math.max(-1, Math.min(1, -m20));
  if (Math.abs(sy) < 1 - 1e-12) {
    return {
      x: Math.atan2(m21, m22) / d,
      y: Math.asin(sy) / d,
      z: Math.atan2(m10, m00) / d,
    };
  }
  // Gimbal lock: fold Z into X (row 1 = [0, cx, -sx]).
  return { x: Math.atan2(-m12, m11) / d, y: Math.sign(sy) * 90, z: 0 };
}

/**
 * Walk down from `rootId` through any `Translate`/`Rotate`/`Scale` wrappers,
 * accumulating their placement, until `match` returns a non-null value for the
 * current node's op. Returns that value, the node it was found at, and the
 * accumulated placement — or `null` if a non-matching, non-transform op (or a
 * missing node) is reached first.
 *
 * Like the hand-rolled walkers it replaces, it keeps the LAST value of each
 * transform kind (a single Translate/Rotate/Scale wrapper — the shape the app
 * and native FFI emit), and checks `match` BEFORE descending so a matching
 * root stops the walk immediately.
 */
export function findWrappedRoot<T>(
  rootId: NodeId,
  nodes: Record<string, Node>,
  match: (op: CsgOp) => T | null,
): { value: T; node: NodeId; transform: TransformInfo } | null {
  const transform = identityTransform();
  let current = rootId;
  while (true) {
    const node = nodes[String(current)];
    if (!node) return null;
    const value = match(node.op);
    if (value !== null) return { value, node: current, transform };
    const op = node.op;
    if (op.type === "Translate") {
      transform.translate = op.offset;
      current = op.child;
    } else if (op.type === "Rotate") {
      transform.rotate = rotateOpToPlacementEuler(op.angles);
      current = op.child;
    } else if (op.type === "Scale") {
      transform.scale = op.factor;
      current = op.child;
    } else {
      return null;
    }
  }
}
