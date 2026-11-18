#!/usr/bin/env python3
"""Generate examples/strandbeest.vcad: three Jansen legs on one crank.

Theo Jansen's "holy numbers" (mm). Each leg is 6 links on a shared crank
disc (pins 120 deg apart); every joint but the crank is `passive` and each leg
closes three loops (at C, and twice at D), so forward kinematics solves the
whole leg from the crank angle alone.
"""
import json, math, sys

a, b, c, d, e, f, g, h, i, j, k, l, m = 38, 41.5, 39.3, 40.1, 55.8, 39.4, 36.7, 65.7, 49, 50, 61.9, 7.8, 15


def cc(p, r1, q, r2, s):
    dx, dy = q[0] - p[0], q[1] - p[1]
    D = math.hypot(dx, dy)
    A = (r1 * r1 - r2 * r2 + D * D) / (2 * D)
    H = math.sqrt(max(0.0, r1 * r1 - A * A))
    mx, my = p[0] + A * dx / D, p[1] + A * dy / D
    return (mx + s * H * dy / D, my - s * H * dx / D)


O, B = (0.0, 0.0), (-a, -l)
LIFT = 0.0  # set below, once the foot path is known


def leg_points(theta):
    A = (m * math.cos(theta), m * math.sin(theta))
    C = cc(A, j, B, b, 1)
    D = cc(A, k, B, c, -1)
    E = cc(B, d, C, e, -1)
    F = cc(E, f, D, g, 1)
    G = cc(F, h, D, i, 1)
    return dict(A=A, C=C, D=D, E=E, F=F, G=G)


LIFT = 2.0 - min(leg_points(t * 2 * math.pi / 360)["G"][1] for t in range(360))
nodes, part_defs, instances, joints, roots = {}, {}, [], [], []
nid = [0]


def node(op, name=None):
    nid[0] += 1
    n = {"id": nid[0], "op": op}
    if name:
        n["name"] = name
    nodes[str(nid[0])] = n
    return nid[0]


def bar(length, z, width=4.0, thick=3.0, angle=0.0, at=(0.0, 0.0)):
    """A bar from `at` along `angle` (deg) of `length`, at height z."""
    cube = node({"type": "Cube", "size": {"x": length + width, "y": width, "z": thick}})
    t0 = node({"type": "Translate", "child": cube, "offset": {"x": -width / 2, "y": -width / 2, "z": z}})
    r = node({"type": "Rotate", "child": t0, "angles": {"x": 0.0, "y": 0.0, "z": angle}})
    return node({"type": "Translate", "child": r, "offset": {"x": at[0], "y": at[1], "z": 0.0}})


def union(ids):
    cur = ids[0]
    for x in ids[1:]:
        cur = node({"type": "Union", "left": cur, "right": x})
    return cur


def local(p, origin, phi):
    dx, dy = p[0] - origin[0], p[1] - origin[1]
    cs, sn = math.cos(-phi), math.sin(-phi)
    return (dx * cs - dy * sn, dx * sn + dy * cs)


def v3(p, z=0.0):
    return {"x": p[0], "y": p[1], "z": z}


def body(pid, name, root, mat):
    part_defs[pid] = {"id": pid, "name": name, "root": root, "defaultMaterial": mat}
    inst = {"id": pid + "_i", "name": name, "partDefId": pid}
    if pid == "frame":
        # Stand it up: the linkage is drawn in XY, so tip the frame onto XZ
        # (kernel Rx(90): y -> z) and lift it so the lowest foot touches z=0.
        inst["transform"] = {"translation": {"x": 0.0, "y": 0.0, "z": LIFT},
                             "rotation": {"x": 90.0, "y": 0.0, "z": 0.0},
                             "scale": {"x": 1.0, "y": 1.0, "z": 1.0}}
    instances.append(inst)
    roots.append({"root": root, "material": mat})


frames = {}  # instance id -> (origin, phi)


def revolute(jid, name, parent, parent_local, child, phi_child, passive, state=None):
    pphi = frames[parent][1] if parent else 0.0
    st = math.degrees(phi_child - pphi) if state is None else state
    jn = {"id": jid, "name": name, "kind": {"type": "Revolute", "axis": {"x": 0.0, "y": 0.0, "z": 1.0}},
          "state": st, "parentInstanceId": parent, "childInstanceId": child,
          "parentAnchor": v3(parent_local), "childAnchor": v3((0.0, 0.0))}
    if passive:
        jn["passive"] = True
    joints.append(jn)


def closure(jid, parent, parent_local, child, child_local):
    joints.append({"id": jid, "name": jid, "kind": {"type": "Revolute", "axis": {"x": 0.0, "y": 0.0, "z": 1.0}},
                   "state": 0.0, "parentInstanceId": parent, "childInstanceId": child,
                   "parentAnchor": v3(parent_local), "childAnchor": v3(child_local), "passive": True})


THICK, R_LINK, R_HOLE, R_PIN = 3.0, 3.6, 1.7, 1.5
pins = {}    # body instance -> list of local pin points
zbody = {}   # body instance -> plate base z


def plate(pts, z, r=R_LINK, holes=True):
    """Rounded convex plate around local points, bored at each one."""
    cx = sum(p[0] for p in pts) / len(pts)
    cy = sum(p[1] for p in pts) / len(pts)
    ring = sorted(pts, key=lambda p: math.atan2(p[1] - cy, p[0] - cx))
    n = len(ring)
    norms = []
    for k in range(n):
        p0, p1 = ring[k], ring[(k + 1) % n]
        dx, dy = p1[0] - p0[0], p1[1] - p0[1]
        L = math.hypot(dx, dy)
        norms.append((dy / L, -dx / L))
    segs = []
    pt = lambda x, y: {"x": x, "y": y}
    for k in range(n):
        p = ring[k]
        nprev, ncur = norms[k - 1], norms[k]
        a0 = (p[0] + r * nprev[0], p[1] + r * nprev[1])
        a1 = (p[0] + r * ncur[0], p[1] + r * ncur[1])
        segs.append({"type": "Arc", "start": pt(*a0), "end": pt(*a1), "center": pt(*p), "ccw": True})
        q = ring[(k + 1) % n]
        segs.append({"type": "Line", "start": pt(*a1), "end": pt(q[0] + r * ncur[0], q[1] + r * ncur[1])})
    hole_loops = []
    if holes:
        for p in pts:
            ring_pts = [(p[0] + R_HOLE * math.cos(2 * math.pi * t / 20), p[1] + R_HOLE * math.sin(2 * math.pi * t / 20)) for t in range(20)]
            hole_loops.append([{"type": "Line", "start": pt(*ring_pts[t]), "end": pt(*ring_pts[(t + 1) % 20])} for t in range(20)])
    if len(ring) == 3:
        # Lighten a triangle plate into a frame: a cutout bounded by each
        # edge offset inward, like the PVC trusses on Jansen's machines.
        d = R_LINK + 1.2
        lines = []
        for k in range(3):
            p0 = ring[k]
            nx, ny = norms[k]
            lines.append(((p0[0] - d * nx, p0[1] - d * ny), (-ny, nx)))
        def meet(l1, l2):
            (p1, d1), (p2, d2) = l1, l2
            den = d1[0] * d2[1] - d1[1] * d2[0]
            t = ((p2[0] - p1[0]) * d2[1] - (p2[1] - p1[1]) * d2[0]) / den
            return (p1[0] + t * d1[0], p1[1] + t * d1[1])
        tri = [meet(lines[k - 1], lines[k]) for k in range(3)]
        # Only if the cutout is still a sensible triangle inside the plate.
        area = 0.5 * abs((tri[1][0] - tri[0][0]) * (tri[2][1] - tri[0][1]) - (tri[2][0] - tri[0][0]) * (tri[1][1] - tri[0][1]))
        if area > 40:
            hole_loops.append([{"type": "Line", "start": pt(*tri[t]), "end": pt(*tri[(t + 1) % 3])} for t in range(3)])
    sk = node({"type": "Sketch2D", "origin": {"x": 0.0, "y": 0.0, "z": z}, "x_dir": {"x": 1.0, "y": 0.0, "z": 0.0},
               "y_dir": {"x": 0.0, "y": 1.0, "z": 0.0}, "segments": segs, "holes": hole_loops})
    return node({"type": "Extrude", "sketch": sk, "direction": {"x": 0.0, "y": 0.0, "z": THICK}})


def add_pin(inst, local_pt):
    lst = pins.setdefault(inst, [])
    if all(math.hypot(local_pt[0] - q[0], local_pt[1] - q[1]) > 0.5 for q in lst):
        lst.append(local_pt)


# Frame: a spine from the crank bearing O to the hip pivot B.
body("frame", "Frame", plate([O, B], -6.0, r=6.0), "titanium")
frames["frame_i"] = ((0.0, 0.0), 0.0)
zbody["frame_i"] = -6.0
# Crank disc.
crank_root = node({"type": "Cylinder", "radius": m + 5, "height": THICK, "segments": 64}, "Crank")
body("crank", "Crank", crank_root, "titanium")
frames["crank_i"] = ((0.0, 0.0), 0.0)
zbody["crank_i"] = 0.0
revolute("crank_axle", "Crank", "frame_i", O, "crank_i", 0.0, False, state=0.0)

for leg in range(3):
    off = leg * 2 * math.pi / 3
    P = leg_points(off)
    z = 4.0 + leg * 14.0
    s = f"_{leg}"

    def mk(pid, name, pivot, ref, extra=(), zz=0.0):
        phi = math.atan2(ref[1] - pivot[1], ref[0] - pivot[0])
        pts = [(0.0, 0.0), local(ref, pivot, phi)] + [local(v, pivot, phi) for v in extra]
        body(pid + s, name + f" {leg + 1}", plate(pts, z + zz), "aluminum")
        inst = pid + s + "_i"
        frames[inst] = (pivot, phi)
        zbody[inst] = z + zz
        add_pin(inst, (0.0, 0.0))
        return phi

    pin_local = (m * math.cos(off), m * math.sin(off))
    phi = mk("j", "Link j", P["A"], P["C"], zz=3.5)
    revolute("j" + s, f"j{leg + 1}", "crank_i", pin_local, "j" + s + "_i", phi, True)
    phi = mk("bce", "Hip", B, P["C"], extra=[P["E"]])
    revolute("bce" + s, f"hip{leg + 1}", "frame_i", B, "bce" + s + "_i", phi, True)
    phi = mk("k", "Link k", P["A"], P["D"], zz=7.0)
    revolute("k" + s, f"k{leg + 1}", "crank_i", pin_local, "k" + s + "_i", phi, True)
    phi = mk("c", "Link c", B, P["D"], zz=3.5)
    revolute("c" + s, f"c{leg + 1}", "frame_i", B, "c" + s + "_i", phi, True)
    fr = frames["bce" + s + "_i"]
    phi = mk("f", "Link f", P["E"], P["F"], zz=3.5)
    revolute("f" + s, f"f{leg + 1}", "bce" + s + "_i", local(P["E"], *fr), "f" + s + "_i", phi, True)
    phi = mk("dfg", "Foot", P["F"], P["D"], extra=[P["G"]])
    revolute("dfg" + s, f"foot{leg + 1}", "f" + s + "_i", (f, 0.0), "dfg" + s + "_i", phi, True)
    closure("close_C" + s, "bce" + s + "_i", (b, 0.0), "j" + s + "_i", (j, 0.0))
    closure("close_Dk" + s, "c" + s + "_i", (c, 0.0), "k" + s + "_i", (k, 0.0))
    closure("close_Df" + s, "c" + s + "_i", (c, 0.0), "dfg" + s + "_i", (g, 0.0))
    # The foot tip gets a rubber pad instead of a pin.
    add_pin("c" + s + "_i", (c, 0.0))
    add_pin("bce" + s + "_i", (b, 0.0))

# Pins: one steel pin through every pivot a body carries, welded to it.
for inst, pts in pins.items():
    for n, p in enumerate(pts):
        pid = f"pin_{inst[:-2]}_{n}"
        cyl = node({"type": "Cylinder", "radius": R_PIN, "height": THICK + 5.0, "segments": 24})
        root = node({"type": "Translate", "child": cyl, "offset": {"x": 0.0, "y": 0.0, "z": zbody[inst] - 2.5}})
        part_defs[pid] = {"id": pid, "name": "Pin", "root": root, "defaultMaterial": "steel"}
        instances.append({"id": pid + "_i", "name": f"Pin ({inst[:-2]})", "partDefId": pid})
        roots.append({"root": root, "material": "steel"})
        joints.append({"id": "weld_" + pid, "name": "weld " + pid, "kind": {"type": "Fixed"}, "state": 0.0,
                       "parentInstanceId": inst, "childInstanceId": pid + "_i",
                       "parentAnchor": v3(p), "childAnchor": v3((0.0, 0.0))})

doc = {"version": "0.1", "nodes": nodes, "materials": {}, "part_materials": {}, "partDefs": part_defs,
       "instances": instances, "joints": joints, "roots": roots, "groundInstanceId": "frame_i"}
out = sys.argv[1] if len(sys.argv) > 1 else "examples/strandbeest.vcad"
json.dump(doc, open(out, "w"), indent=1)
print(f"{out}: {len(instances)} instances, {len(joints)} joints ({sum(1 for x in joints if x.get('passive'))} passive)")
