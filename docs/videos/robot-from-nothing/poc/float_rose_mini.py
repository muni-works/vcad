#!/usr/bin/env python3
"""Make an exported rose-mini.vcad simulable: the hand-patch vcad can't express in loon yet.

  python3 float_rose_mini.py rose-mini.vcad rose-mini-float.vcad

What loon export leaves out (crates/vcad-loon/src/convert.rs), and what this adds:
  1. a Free base: the loon assembly grounds the torso (`groundInstanceId` = cover-chest_i),
     so the robot is welded to the world. We add a world instance + Free joint, as the K1
     URDF import does (examples/k1-floating.vcad).
  2. material densities: loon materials carry density None -> physics falls back to
     1000 kg/m^3 for every part. Values below are generic handbook densities.
  3. effort / velocity limits: loon revolutes carry None. Values are the RobStride peaks
     the rose repo sizes against (rose/body/quad.py MOTORS: rs00 14, rs02 17, rs06 36 N*m).
"""
import json, sys

src, dst = sys.argv[1], sys.argv[2]
d = json.load(open(src))

DENSITY = {  # kg/m^3
    'aluminium': 2700, 'steel': 7850, 'heatsink': 2700, 'motor_dark': 3500,  # motor body: Al shell + Cu/Fe core, lumped
    'satin_black': 1240, 'black_smooth': 1240, 'cable_black': 1400, 'sole_rubber': 1100,
    'pcb_green': 1850, 'lens': 1190, 'glass': 2500, 'face': 1190, 'dots': 1190,
    'cell_pack': 2300, 'default': 1240,
}
for name, m in d['materials'].items():
    m['density'] = DENSITY.get(name, 1240)

# RobStride per joint, from rose/body/k1_teardown.py OURS (mini: big=rs06, yaw=rs02, arm/ankle/head=rs00)
PEAK = {'rs00': (14.0, 33.0 * 360 / 6.283), 'rs02': (17.0, 41.0 * 360 / 6.283), 'rs06': (36.0, 50.0 * 360 / 6.283)}
def motor(name):
    if any(k in name for k in ('hip_roll', 'hip_pitch', 'knee')): return 'rs06'
    if 'hip_yaw' in name: return 'rs02'
    return 'rs00'
for j in d['joints']:
    k = j['kind']
    if k['type'] == 'Revolute':
        tau, vel = PEAK[motor(j['name'])]
        k['effort_limit'] = tau
        k['velocity_limit'] = vel  # deg/s; no-load speed placeholder, not a datasheet value

base = d['groundInstanceId']
d['partDefs']['part_world'] = {'id': 'part_world', 'name': 'world', 'root': 999999, 'defaultMaterial': 'default'}
nodes = d['nodes']
world_node = {'id': 999999, 'name': 'world', 'op': {'type': 'Cube', 'size': {'x': 0.01, 'y': 0.01, 'z': 0.01}}}
if isinstance(nodes, dict):
    nodes['999999'] = world_node
else:
    nodes.append(world_node)
d['instances'].insert(0, {'id': 'world_inst', 'partDefId': 'part_world', 'name': 'world'})
d['joints'].insert(0, {
    'id': 'world_joint', 'name': 'world_joint', 'parentInstanceId': 'world_inst', 'childInstanceId': base,
    'parentAnchor': {'x': 0.0, 'y': 0.0, 'z': 589.2}, 'childAnchor': {'x': 0.0, 'y': 0.0, 'z': 0.0},
    'kind': {'type': 'Free'}, 'state': 0.0,
})
d['groundInstanceId'] = 'world_inst'
json.dump(d, open(dst, 'w'))
rev = [j['name'] for j in d['joints'] if j['kind']['type'] == 'Revolute']
print(f'base={base} revolutes={len(rev)} materials={len(d["materials"])} -> {dst}')
