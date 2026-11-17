/**
 * Coplanar face merge through the WASM kernel: a boolean leaves a flat
 * surface in pieces, and what leaves evaluation for a root has one face per
 * flat region — in the Rust evaluator (`evaluateDocument`,
 * `inspectDocumentFaces`), in the TS fallback evaluator, and on a bare
 * `Solid` via `mergeCoplanarFaces`.
 */
import { beforeAll, describe, expect, it } from "vitest";
import type { Document, Node } from "@vcad/ir";
import { createDocument } from "@vcad/ir";
import { evaluateDocumentTS } from "../evaluate.js";
import { getKernelWasm } from "../wasm-singleton.js";

type SolidHandle = {
  union(other: SolidHandle): SolidHandle;
  translate(x: number, y: number, z: number): SolidHandle;
  volume(): number;
  mergeCoplanarFaces?: () => SolidHandle;
};
type Kernel = {
  Solid: { cube(x: number, y: number, z: number): SolidHandle };
  inspectDocumentFaces?: (docJson: string) => string;
};

/** Two 10 mm cubes side by side, flush on four sides, unioned. */
function flushBoxes(): Document {
  const nodes: Node[] = [
    { id: 1, name: "a", op: { type: "Cube", size: { x: 10, y: 10, z: 10 } } },
    { id: 2, name: "b0", op: { type: "Cube", size: { x: 10, y: 10, z: 10 } } },
    { id: 3, name: "b", op: { type: "Translate", child: 2, offset: { x: 10, y: 0, z: 0 } } },
    { id: 4, name: "block", op: { type: "Union", left: 1, right: 3 } },
  ];
  const doc = createDocument();
  for (const n of nodes) doc.nodes[String(n.id)] = n;
  doc.roots.push({ root: 4, material: "default" });
  return doc;
}

let kernel: Kernel;
let hasMerge = false;
beforeAll(async () => {
  kernel = (await getKernelWasm()) as unknown as Kernel;
  hasMerge = typeof kernel.Solid.cube(1, 1, 1).mergeCoplanarFaces === "function";
  // Checked-in artifacts can predate the binding; CI builds the kernel from
  // source and must never skip.
  if (!hasMerge && process.env.CI) {
    throw new Error("kernel WASM lacks Solid.mergeCoplanarFaces");
  }
});

describe("coplanar face merge (WASM)", () => {
  it("a root's faces are one per flat region", () => {
    if (!hasMerge || !kernel.inspectDocumentFaces) return;
    const out = JSON.parse(kernel.inspectDocumentFaces(JSON.stringify(flushBoxes())));
    const part = out.parts[0];
    expect(part.brep).toBe(true);
    // Top, bottom, front and back are each one rectangle again.
    expect(part.report.faces).toHaveLength(6);
  });

  it("Solid.mergeCoplanarFaces returns the same solid", () => {
    if (!hasMerge) return;
    const { Solid } = kernel;
    const split = Solid.cube(10, 10, 10).union(Solid.cube(10, 10, 10).translate(10, 0, 0));
    const merged = split.mergeCoplanarFaces!();
    expect(merged.volume()).toBeCloseTo(2000, 6);
    expect(split.volume()).toBeCloseTo(2000, 6);
  });

  it("the TS fallback evaluator hands out the merged solid", () => {
    if (!hasMerge) return;
    const scene = evaluateDocumentTS(flushBoxes(), kernel as never);
    expect(scene.parts).toHaveLength(1);
    const solid = scene.parts[0].solid as unknown as SolidHandle;
    expect(solid.volume()).toBeCloseTo(2000, 6);
    expect(scene.parts[0].mesh.indices.length).toBeGreaterThan(0);
    expect(scene.failures ?? []).toHaveLength(0);
  });
});
