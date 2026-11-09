/**
 * `assemble` — add instances and joints to a session in place. `create` and
 * `create_cad_loon {mode:"add"}` add parts; this connects them, against
 * whatever is already in the document, without re-authoring any of it.
 */
import type { Document, Instance, Joint, JointKind, PartDef, Transform3D, Vec3 } from "@vcad/ir";
import { rootForPartId } from "./part-ids.js";
import { getSession } from "./session-core.js";
import { behavior, type ToolDef } from "./tool-def.js";

const vec3Schema = {
  type: "object" as const,
  properties: { x: { type: "number" as const }, y: { type: "number" as const }, z: { type: "number" as const } },
  required: ["x", "y", "z"],
};

interface InstanceArg {
  id: string;
  part: string;
  name?: string;
  at?: Vec3;
  transform?: Transform3D;
  material?: string;
}

interface JointArg {
  id?: string;
  name?: string;
  parent?: string | null;
  child: string;
  parent_anchor?: Vec3;
  child_anchor?: Vec3;
  kind: JointKind;
  state?: number;
}

const ZERO: Vec3 = { x: 0, y: 0, z: 0 };
const JOINT_TYPES = new Set(["Fixed", "Revolute", "Slider", "Cylindrical", "Ball"]);

const fail = (why: string): never => {
  throw new Error(`assemble: ${why} Nothing was changed.`);
};

/**
 * Apply one `assemble` call. Everything is worked out on copies and installed
 * at the end, so a bad joint at the end of the list leaves the document alone.
 * A loose part an instance names becomes a part definition and leaves the
 * scene roots, since its geometry now reaches the scene through the instance.
 */
export function assembleInto(doc: Document, args: { instances?: InstanceArg[]; joints?: JointArg[]; ground?: string }) {
  const instanceArgs = args.instances ?? [];
  const jointArgs = args.joints ?? [];
  if (!instanceArgs.length && !jointArgs.length && !args.ground) {
    fail("pass at least one of `instances`, `joints`, `ground`.");
  }
  const partDefs: Record<string, PartDef> = { ...doc.partDefs };
  const instances: Instance[] = [...(doc.instances ?? [])];
  const joints: Joint[] = [...(doc.joints ?? [])];
  let roots = [...doc.roots];
  const partIds = { ...doc.partIds };
  const defined = new Map<string, string>();

  const defFor = (part: string): string => {
    const known = defined.get(part);
    if (known) return known;
    const existing = Object.entries(partDefs).find(([k, d]) => k === part || d.name === part);
    if (existing) return existing[0];
    const entry = rootForPartId(doc, part)?.entry ?? roots.find((r) => doc.nodes[String(r.root)]?.name === part);
    if (!entry || !roots.includes(entry)) {
      return fail(`no part or part definition "${part}".`);
    }
    if (doc.nodes[String(entry.root)]?.op.type === "PcbBoard") fail("a board can't be an assembly instance.");
    const name = doc.nodes[String(entry.root)]?.name ?? part;
    let id = name.replace(/\W+/g, "_");
    for (let n = 2; partDefs[id]; n++) id = `${name.replace(/\W+/g, "_")}_${n}`;
    partDefs[id] = { id, name, root: entry.root, defaultMaterial: entry.material };
    roots = roots.filter((r) => r !== entry);
    delete partIds[String(entry.root)];
    defined.set(part, id);
    return id;
  };

  for (const a of instanceArgs) {
    if (!a.id) fail("every instance needs an `id`.");
    if (instances.some((i) => i.id === a.id)) fail(`instance "${a.id}" already exists.`);
    instances.push({
      id: a.id,
      partDefId: defFor(a.part),
      name: a.name ?? a.id,
      ...(a.material ? { material: a.material } : {}),
      ...(a.transform
        ? { transform: a.transform }
        : a.at
          ? { transform: { translation: a.at, rotation: ZERO, scale: { x: 1, y: 1, z: 1 } } }
          : {}),
    } as Instance);
  }

  // The engine renumbers instance ids whenever it rebuilds the document, so
  // the name an instance was given is the handle that lasts.
  const instanceId = (ref: string): string | undefined =>
    (instances.find((i) => i.id === ref) ?? instances.find((i) => i.name === ref))?.id;
  const ordinal = joints.length;
  jointArgs.forEach((a, n) => {
    const child = instanceId(a.child) ?? fail(`joint child "${a.child}" is not an instance.`);
    const parent =
      a.parent == null ? null : (instanceId(a.parent) ?? fail(`joint parent "${a.parent}" is not an instance.`));
    if (parent === child) fail(`joint on "${a.child}" has itself as parent.`);
    if (joints.some((j) => j.childInstanceId === child)) fail(`"${a.child}" already has a joint driving it.`);
    if (!JOINT_TYPES.has(a.kind?.type)) fail(`joint kind must be one of ${[...JOINT_TYPES].join(", ")}.`);
    if (a.kind.type !== "Fixed" && a.kind.type !== "Ball" && !("axis" in a.kind && a.kind.axis)) {
      fail(`a ${a.kind.type} joint needs an axis.`);
    }
    const id = a.id ?? `joint${ordinal + n + 1}`;
    if (joints.some((j) => j.id === id)) fail(`joint "${id}" already exists.`);
    joints.push({
      id,
      ...(a.name ? { name: a.name } : {}),
      parentInstanceId: parent,
      childInstanceId: child,
      parentAnchor: a.parent_anchor ?? ZERO,
      childAnchor: a.child_anchor ?? ZERO,
      kind: a.kind,
      state: a.state ?? 0,
    } as Joint);
  });
  // A chain looping back on itself has no root to solve from.
  for (const start of joints.slice(ordinal)) {
    const seen = new Set<string>([start.childInstanceId]);
    let at: string | null | undefined = start.parentInstanceId;
    while (at) {
      if (seen.has(at)) fail(`joints form a loop through "${at}".`);
      seen.add(at);
      const here: string = at;
      at = joints.find((j) => j.childInstanceId === here)?.parentInstanceId;
    }
  }
  const ground = args.ground ? (instanceId(args.ground) ?? fail(`ground "${args.ground}" is not an instance.`)) : undefined;

  doc.partDefs = partDefs;
  doc.instances = instances;
  doc.joints = joints;
  doc.roots = roots;
  if (doc.partIds) doc.partIds = partIds;
  if (ground) doc.groundInstanceId = ground;
  return {
    part_defs_created: [...defined.values()],
    instances: instanceArgs.map((a) => a.id),
    joints: joints.slice(ordinal).map((j) => j.id),
  };
}

export const toolDefs: ToolDef[] = [
  {
    name: "assemble",
    pack: null,
    description:
      "Add instances and joints to a document without rebuilding it. Everything already there " +
      "(parts, board, other joints) stays. An instance names an existing part id, part name, or " +
      "part definition; a loose part it names becomes a part definition, shown through its " +
      "instance. To add new geometry first, use `create` or `create_cad_loon` with mode:\"add\". " +
      "All-or-nothing: an invalid entry fails the call with the document unchanged.",
    inputSchema: {
      type: "object" as const,
      properties: {
        document_id: { type: "string", description: "Session to add to." },
        instances: {
          type: "array",
          items: {
            type: "object",
            properties: {
              id: { type: "string", description: "New instance id." },
              part: { type: "string", description: "Part id, part name, or part definition." },
              name: { type: "string" },
              at: { ...vec3Schema, description: "Translation in mm. Identity when omitted: the part stays where it is." },
              transform: { type: "object", description: "Full {translation, rotation, scale}; overrides `at`." },
              material: { type: "string" },
            },
            required: ["id", "part"],
          },
        },
        joints: {
          type: "array",
          items: {
            type: "object",
            properties: {
              id: { type: "string" },
              name: { type: "string" },
              parent: { type: ["string", "null"], description: "Parent instance, by id or name; null grounds it to the world." },
              child: { type: "string", description: "Child instance, by id or name." },
              parent_anchor: vec3Schema,
              child_anchor: vec3Schema,
              kind: {
                type: "object",
                description:
                  'E.g. {"type":"Revolute","axis":{"x":0,"y":0,"z":1},"limits":[-90,90]}. ' +
                  "type: Fixed, Revolute, Slider, Cylindrical, Ball.",
              },
              state: { type: "number", description: "Initial angle (deg) or position (mm)." },
            },
            required: ["child", "kind"],
          },
        },
        ground: { type: "string", description: "Instance to ground." },
      },
      required: ["document_id"],
    },
    handler: (args) => {
      const id = String(args.document_id ?? "");
      const added = assembleInto(getSession(id), args as Parameters<typeof assembleInto>[1]);
      return { content: [{ type: "text" as const, text: JSON.stringify({ document_id: id, ...added }) }] };
    },
    behavior: behavior({ writesDoc: true, geometry: true }),
  },
];
