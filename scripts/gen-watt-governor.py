#!/usr/bin/env python3
"""Generate examples/watt-governor.vcad: James Watt's centrifugal governor.

A vertical spindle carries a head; two arms hang from it with brass balls at
their ends; two links run from the arms down to a sleeve that slides on the
spindle. Kinematically the arms, links and sleeve form two closed loops, so
the whole linkage has ONE free coordinate besides the spindle angle: the
sleeve height. The app's governor behavior (joint names "Spindle" and
"Sleeve") sets that height from spindle speed, cos(phi) = g / (w^2 L) — spin
it and the balls fly out and lift the sleeve.
"""
import json, math, sys

L, A, R_OFF, TOP = 70.0, 40.0, 6.0, 160.0   # arm length, link attach, pivot offset, pivot height
S_REST = 3.0                                 # sleeve lift at rest (keeps the arms off the toggle)
PHI0 = math.degrees(math.acos(1 - S_REST / (2 * A)))
nodes, part_defs, instances, joints, roots = {}, {}, [], [], []
nid = [0]


def node(op):
    nid[0] += 1
    nodes[str(nid[0])] = {"id": nid[0], "op": op}
    return nid[0]


def tr(child, x=0.0, y=0.0, z=0.0):
    return node({"type": "Translate", "child": child, "offset": {"x": x, "y": y, "z": z}})


def rot(child, x=0.0, y=0.0, z=0.0):
    return node({"type": "Rotate", "child": child, "angles": {"x": x, "y": y, "z": z}})


def cyl(r, h, z0=0.0, seg=48):
    return tr(node({"type": "Cylinder", "radius": r, "height": h, "segments": seg}), z=z0)


def body(pid, name, root, mat, transform=None):
    part_defs[pid] = {"id": pid, "name": name, "root": root, "defaultMaterial": mat}
    inst = {"id": pid + "_i", "name": name, "partDefId": pid}
    if transform:
        inst["transform"] = transform
    instances.append(inst)
    roots.append({"root": root, "material": mat})


v3 = lambda x, y, z: {"x": x, "y": y, "z": z}


def joint(jid, name, kind, parent, child, pa, ca, state=0.0, passive=False):
    j = {"id": jid, "name": name, "kind": kind, "state": state, "parentInstanceId": parent,
         "childInstanceId": child, "parentAnchor": v3(*pa), "childAnchor": v3(*ca)}
    if passive:
        j["passive"] = True
    joints.append(j)


rev_y = {"type": "Revolute", "axis": v3(0.0, 1.0, 0.0)}
fixed = {"type": "Fixed"}

# Base: a round foot and a column bearing the spindle.
body("base", "Base", cyl(55, 10), "walnut")
body("column", "Column", cyl(9, 40, 10), "steel")
joint("weld_column", "weld column", fixed, "base_i", "column_i", (0, 0, 0), (0, 0, 0))
# Spindle with a head at the top.
body("spindle", "Spindle", cyl(4, TOP + 20, 10, 32), "steel")
joint("spindle", "Spindle", {"type": "Revolute", "axis": v3(0, 0, 1)}, "base_i", "spindle_i", (0, 0, 0), (0, 0, 0))
body("head", "Head", cyl(12, 14, TOP - 4), "brass")
joint("weld_head", "weld head", fixed, "spindle_i", "head_i", (0, 0, 0), (0, 0, 0))
# Sleeve: slides on the spindle (the free coordinate).
SLEEVE0 = TOP - 2 * A                      # sleeve collar height with arms hanging
body("sleeve", "Sleeve", cyl(10, 12, -6), "brass")
joint("sleeve", "Sleeve", {"type": "Slider", "axis": v3(0, 0, 1), "limits": [S_REST, 2 * A - 6]},
      "spindle_i", "sleeve_i", (0, 0, SLEEVE0), (0, 0, 0), state=S_REST)

for side, sx in (("L", -1.0), ("R", 1.0)):
    # Arm: hangs along -z from its pivot; rod plus a ball at the end.
    body("arm" + side, f"Arm {side}", cyl(2.2, L, -L, 24), "steel")
    joint("arm" + side, f"arm {side}", rev_y, "spindle_i", "arm" + side + "_i",
          (sx * R_OFF, 0, TOP), (0, 0, 0), state=-sx * PHI0, passive=True)
    ball = node({"type": "Sphere", "radius": 14.0, "segments": 48})
    body("ball" + side, f"Ball {side}", tr(ball, z=-L), "brass")
    joint("weld_ball" + side, "weld ball " + side, fixed, "arm" + side + "_i", "ball" + side + "_i", (0, 0, 0), (0, 0, 0))
    # Link: from the sleeve collar up to the arm.
    body("link" + side, f"Link {side}", cyl(1.8, A, 0, 24), "steel")
    joint("link" + side, f"link {side}", rev_y, "sleeve_i", "link" + side + "_i",
          (sx * R_OFF, 0, 0), (0, 0, 0), state=sx * PHI0, passive=True)
    joint("close" + side, f"close {side}", rev_y, "arm" + side + "_i", "link" + side + "_i",
          (0, 0, -A), (0, 0, A), passive=True)

doc = {"version": "0.1", "nodes": nodes, "materials": {}, "part_materials": {}, "partDefs": part_defs,
       "instances": instances, "joints": joints, "roots": roots, "groundInstanceId": "base_i"}
out = sys.argv[1] if len(sys.argv) > 1 else "examples/watt-governor.vcad"
json.dump(doc, open(out, "w"), indent=1)
print(f"{out}: {len(instances)} instances, {len(joints)} joints")
