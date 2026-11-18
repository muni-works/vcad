#!/usr/bin/env python3
"""Generate examples/antikythera.vcad: the Antikythera mechanism's moon train.

Freeth et al.'s reconstruction of the front lunar train: arbor b (one turn per
year, carries the sun pointer and the 224-tooth main wheel) drives
b2(64)->c1(38), c2(48)->d1(24), d2(127)->e2(32). The moon pointer turns
64/38 * 48/24 * 127/32 = 254/19 times per year: 254 sidereal months in 19
years, the Metonic relation. Arbor e comes back coaxial with b, as in the
original, so the sun and moon pointers share one dial. Couplings are written
explicitly with tooth-into-gap phase (vcad_ir::gear::mesh_rotation_deg).
"""
import json, math, sys
MOD = 0.6
nodes, part_defs, instances, joints, roots = {}, {}, [], [], []
nid = [0]


def node(op, name=None):
    nid[0] += 1
    n = {"id": nid[0], "op": op}
    if name:
        n["name"] = name
    nodes[str(nid[0])] = n
    return nid[0]


def gear(z, zoff, width=2.5, bore=0.0):
    g = node({"type": "SpurGear", "module": MOD, "teeth": z, "pressure_angle_deg": 20.0, "face_width": width,
              "bore_diameter": bore, "backlash": 0.05})
    return node({"type": "Translate", "child": g, "offset": {"x": 0.0, "y": 0.0, "z": zoff}})


def union(ids):
    cur = ids[0]
    for x in ids[1:]:
        cur = node({"type": "Union", "left": cur, "right": x})
    return cur


def arbor_shaft(z0, z1):
    c = node({"type": "Cylinder", "radius": 1.5, "height": z1 - z0, "segments": 24})
    return node({"type": "Translate", "child": c, "offset": {"x": 0.0, "y": 0.0, "z": z0}})


def sketch_extrude(segs, z, thick, holes=()):
    sk = node({"type": "Sketch2D", "origin": {"x": 0.0, "y": 0.0, "z": z}, "x_dir": {"x": 1.0, "y": 0.0, "z": 0.0},
               "y_dir": {"x": 0.0, "y": 1.0, "z": 0.0}, "segments": segs, "holes": list(holes)})
    return node({"type": "Extrude", "sketch": sk, "direction": {"x": 0.0, "y": 0.0, "z": thick}})


P2D = lambda x, y: {"x": x, "y": y}


def circle_loop(cx, cy, r, n=48):
    pts = [(cx + r * math.cos(2 * math.pi * t / n), cy + r * math.sin(2 * math.pi * t / n)) for t in range(n)]
    return [{"type": "Line", "start": P2D(*pts[t]), "end": P2D(*pts[(t + 1) % n])} for t in range(n)]


def pointer(length, z, width=3.0):
    """A tapered needle: round boss at the arbor, fine point at the tip."""
    r = width + 1.5
    a0, a1 = math.radians(70), math.radians(290)
    p0 = (r * math.cos(a0), r * math.sin(a0))
    p1 = (r * math.cos(a1), r * math.sin(a1))
    segs = [{"type": "Arc", "start": P2D(*p0), "end": P2D(*p1), "center": P2D(0, 0), "ccw": True},
            {"type": "Line", "start": P2D(*p1), "end": P2D(length, -0.6)},
            {"type": "Line", "start": P2D(length, -0.6), "end": P2D(length, 0.6)},
            {"type": "Line", "start": P2D(length, 0.6), "end": P2D(*p0)}]
    return sketch_extrude(segs, z, 1.4, [circle_loop(0, 0, 1.6, 24)])


def body(pid, name, root, mat):
    part_defs[pid] = {"id": pid, "name": name, "root": root, "defaultMaterial": mat}
    instances.append({"id": pid + "_i", "name": name, "partDefId": pid})
    roots.append({"root": root, "material": mat})


def mesh_rot(z1, rot1, z2, direction):
    t1, t2 = 360.0 / z1, 360.0 / z2
    f = ((direction - rot1) / t1) % 1.0
    return (direction + 180.0 + (f - 0.5) * t2) % 360.0


cd = lambda z1, z2: MOD * (z1 + z2) / 2
# Planes (z) for each mesh.
P0, P1, P2, P3, P4 = 0.0, 4.0, 8.0, 12.0, 16.0
# Layout: b at the origin; c, d, e placed so every pair meshes, and the last
# stage e1(32)->b3(32) lands on a moon tube coaxial with b (Freeth's layout:
# four external meshes, so the moon turns the same way as the sun).
B = (0.0, 0.0)
rc, rcd, rde, rem = cd(64, 38), cd(48, 24), cd(127, 32), cd(32, 32)
C = (rc, 0.0)
al = math.radians(15)
D = (C[0] + rcd * math.cos(al), C[1] + rcd * math.sin(al))
# e: on circle(D, rde) and circle(origin, rem), the branch with y > 0.
dx, dy = D[0], D[1]
dd = math.hypot(dx, dy)
aa = (rem ** 2 - rde ** 2 + dd ** 2) / (2 * dd)
hh = math.sqrt(rem ** 2 - aa ** 2)
E = (aa * dx / dd - hh * dy / dd, aa * dy / dd + hh * dx / dd)

def rounded_rect(x0, y0, x1, y1, r):
    pts = [(x1 - r, y0), (x1, y0 + r), (x1, y1 - r), (x1 - r, y1), (x0 + r, y1), (x0, y1 - r), (x0, y0 + r), (x0 + r, y0)]
    cs = [(x1 - r, y0 + r), (x1 - r, y1 - r), (x0 + r, y1 - r), (x0 + r, y0 + r)]
    segs = []
    for k in range(4):
        a, b = pts[2 * k], pts[2 * k + 1]
        segs.append({"type": "Arc", "start": P2D(*a), "end": P2D(*b), "center": P2D(*cs[k]), "ccw": True})
        c = pts[(2 * k + 2) % 8]
        segs.append({"type": "Line", "start": P2D(*b), "end": P2D(*c)})
    return segs


# Case: a walnut back plate, bored for every arbor, and a brass dial bezel.
holes = [circle_loop(px, py, 2.2, 24) for (px, py) in (B, C, D, E)]
body("plate", "Case", sketch_extrude(rounded_rect(-95, -90, 115, 95, 12), -8.0, 6.0, holes), "walnut")
body("bezel", "Dial bezel", sketch_extrude(circle_loop(0, 0, 74, 96), 30.5, 1.5, [circle_loop(0, 0, 66, 96)]), "brass")
# Each arbor is a bare shaft; its gears and pointer are separate instances
# welded on with Fixed joints (no gear booleans: evaluating a union with an
# involute profile is slow, and a weld is what the real arbor is anyway).
welds = []


def arbor(pid, name, z0, z1, parts, mat="steel", tube=False):
    shaft = sketch_extrude(circle_loop(0, 0, 3.0, 32), z0, z1 - z0, [circle_loop(0, 0, 1.9, 32)]) if tube else arbor_shaft(z0, z1)
    body(pid, name, shaft, mat)
    for k, (kind, arg, zz, pname, pmat) in enumerate(parts):
        if kind == "gear":
            root = gear(arg, zz, bore=4.0 if tube else 0.0)
        elif kind == "ptr":
            root = pointer(arg, zz, 3.0)
        elif kind == "ball":
            length, rad = arg
            root = node({"type": "Translate", "child": node({"type": "Sphere", "radius": rad, "segments": 32}),
                         "offset": {"x": length, "y": 0.0, "z": zz}})
        else:  # crank handle under the case: arm + knob
            root = union([sketch_extrude([
                {"type": "Arc", "start": P2D(0, -4), "end": P2D(0, 4), "center": P2D(0, 0), "ccw": False},
                {"type": "Line", "start": P2D(0, 4), "end": P2D(arg, 4)},
                {"type": "Arc", "start": P2D(arg, 4), "end": P2D(arg, -4), "center": P2D(arg, 0), "ccw": False},
                {"type": "Line", "start": P2D(arg, -4), "end": P2D(0, -4)}], zz, 3.0)]) if False else \
                sketch_extrude([
                {"type": "Arc", "start": P2D(0, 4), "end": P2D(0, -4), "center": P2D(0, 0), "ccw": True},
                {"type": "Line", "start": P2D(0, -4), "end": P2D(arg, -4)},
                {"type": "Arc", "start": P2D(arg, -4), "end": P2D(arg, 4), "center": P2D(arg, 0), "ccw": True},
                {"type": "Line", "start": P2D(arg, 4), "end": P2D(0, 4)}], zz, 3.0)
        sub = f"{pid}_{k}"
        body(sub, pname, root, pmat)
        welds.append((pid + "_i", sub + "_i"))


# The b shaft stops below the moon tube's gear (in the original the moon
# output rides a tube around it); the sun pointer sits between d2 and e1.
# The year arbor runs up through the moon tube to the sun pointer on top.
arbor("b", "Arbor b", -16.0, 28.4, [("gear", 224, P0, "b1 (224) · year wheel", "brass"),
                                 ("gear", 64, P1, "b2 (64)", "brass"), ("ptr", 62, 27.0, "Sun pointer", "gold"),
                                 ("ball", (56, 3.2), 27.7, "Sun", "gold"), ("crank", 34, -15.0, "Crank handle", "walnut")])
arbor("c", "Arbor c", P1, P2 + 2.5, [("gear", 38, P1, "c1 (38)", "brass"), ("gear", 48, P2, "c2 (48)", "brass")])
arbor("d", "Arbor d", P2, P3 + 2.5, [("gear", 24, P2, "d1 (24)", "brass"), ("gear", 127, P3, "d2 (127)", "brass")])
arbor("e", "Arbor e", P3, P4 + 2.5, [("gear", 32, P3, "e2 (32)", "brass"), ("gear", 32, P4, "e1 (32)", "brass")])
arbor("m", "Moon tube", P4, 23.0, [("gear", 32, P4, "b3 (32)", "silver"), ("ptr", 48, 21.5, "Moon pointer", "silver"),
                                 ("ball", (42, 3.6), 22.2, "Moon", "silver")], "silver", tube=True)


def axle(jid, name, inst, at, coupling=None):
    j = {"id": jid, "name": name, "kind": {"type": "Revolute", "axis": {"x": 0.0, "y": 0.0, "z": 1.0}},
         "state": 0.0, "parentInstanceId": "plate_i", "childInstanceId": inst,
         "parentAnchor": {"x": at[0], "y": at[1], "z": 0.0}, "childAnchor": {"x": 0.0, "y": 0.0, "z": 0.0}}
    if coupling:
        j["coupling"] = coupling
    joints.append(j)


ang = lambda p, q: math.degrees(math.atan2(q[1] - p[1], q[0] - p[0]))
axle("b_axle", "Year (crank)", "b_i", B)
# Coupling: driven = ratio * driver + offset. The offset puts tooth into gap
# at the driver's CURRENT state (each arbor's state at year 0 is its offset
# chain), so it is the meshed angle minus ratio * that state.
def meshed(z1, s1, z2, d, ratio):
    return mesh_rot(z1, s1, z2, d) - ratio * s1


s_b = 0.0
r_c = -64 / 38
off_c = meshed(64, s_b, 38, ang(B, C), r_c)
axle("c_axle", "Arbor c", "c_i", C, {"driver": "b_axle", "ratio": r_c, "offset": off_c})
s_c = r_c * s_b + off_c
r_d = -48 / 24
off_d = meshed(48, s_c, 24, ang(C, D), r_d)
axle("d_axle", "Arbor d", "d_i", D, {"driver": "c_axle", "ratio": r_d, "offset": off_d})
s_d = r_d * s_c + off_d
r_e = -127 / 32
off_e = meshed(127, s_d, 32, ang(D, E), r_e)
axle("e_axle", "Arbor e", "e_i", E, {"driver": "d_axle", "ratio": r_e, "offset": off_e})
s_e = r_e * s_d + off_e
off_m = meshed(32, s_e, 32, ang(E, B), -1.0)
axle("moon_axle", "Moon", "m_i", B, {"driver": "e_axle", "ratio": -1.0, "offset": off_m})

for parent, child in welds:
    joints.append({"id": "weld_" + child, "name": "weld " + child, "kind": {"type": "Fixed"}, "state": 0.0,
                   "parentInstanceId": parent, "childInstanceId": child,
                   "parentAnchor": {"x": 0.0, "y": 0.0, "z": 0.0}, "childAnchor": {"x": 0.0, "y": 0.0, "z": 0.0}})

doc = {"version": "0.1", "nodes": nodes, "materials": {}, "part_materials": {}, "partDefs": part_defs,
       "instances": instances, "joints": joints, "roots": roots, "groundInstanceId": "plate_i"}
out = sys.argv[1] if len(sys.argv) > 1 else "examples/antikythera.vcad"
json.dump(doc, open(out, "w"), indent=1)
r = 64 / 38 * 48 / 24 * 127 / 32 * 32 / 32
print(f"{out}: moon/sun = +{r:.6f} (254/19 = {254/19:.6f}); |d| = {math.hypot(*D):.1f} mm, e at ({E[0]:.1f},{E[1]:.1f})")
