#!/usr/bin/env python3
"""Build the teaser's vcad-render inputs from rose-mini and the gym trace.

  python3 make_shots.py rose-mini.vcad rose-mini.joints.json trace.csv outdir/

writes:
  shot_assemble.json  timeline: parts appear link by link, feet up, while the
                      assembly collapses from an exploded view; slow turntable
  shot_cut.json       timeline: arms and head drop out, leaving the legs (rml-12)
  rose-fall.vcad      rose-mini on a hidden slider-x / slider-z / pitch rig over
                      a floor slab, so the gym's base trajectory can be keyframed
                      (vcad-render timelines drive scalar joints, not a Free base)
  shot_fall.json      timeline: the gym trace, base x / z / pitch, at 1/4 speed
                      after a 1 s hold. joint deflections are not drawn: the PD
                      held them near zero and the trace logs the base only.
"""
import csv, json, sys, copy

doc_path, joints_path, trace_path, out = sys.argv[1:5]
doc = json.load(open(doc_path))
inv = json.load(open(joints_path))

part_to_inst = {i['partDefId']: i['id'] for i in doc['instances']}
# --animate refuses a timeline with no joint track; hold one joint at its pose
HOLD_J = doc['joints'][0]
def hold_track(dur):
    v = HOLD_J.get('state', 0.0)
    return {'target': {'type': 'Joint', 'jointId': HOLD_J['id']}, 'keys': [{'t': 0, 'value': v}, {'t': dur, 'value': v}]}
links = inv['links']  # name -> {origin_mm, aabb_mm, parts}


def link_instances(name):
    names = [p['name'] if isinstance(p, dict) else p for p in links[name].get('parts', [])]
    return [part_to_inst[n] for n in names if n in part_to_inst]


# ---- shot 1: assemble, feet up --------------------------------------------
DUR_A = 20.0
order = sorted(links, key=lambda n: links[n]['aabb_mm'][0][2])  # by lowest point
seen, tracks = set(), []
t0, t1 = 0.6, 13.0
for k, name in enumerate(order):
    insts = [i for i in link_instances(name) if i not in seen]
    base = t0 + (t1 - t0) * k / max(1, len(order) - 1)
    for j, inst in enumerate(insts):
        seen.add(inst)
        t = base + 0.04 * j
        tracks.append({'target': {'type': 'Visibility', 'instanceId': inst},
                       'keys': [{'t': 0, 'value': 0}, {'t': t, 'value': 1, 'ease': 'step'}]})
leftover = [i['id'] for i in doc['instances'] if i['id'] not in seen]
for inst in leftover:  # parts the inventory doesn't list arrive with the last link
    tracks.append({'target': {'type': 'Visibility', 'instanceId': inst},
                   'keys': [{'t': 0, 'value': 0}, {'t': t1, 'value': 1, 'ease': 'step'}]})
tracks.append({'target': {'type': 'Explode'},
               'keys': [{'t': 0, 'value': 0.45}, {'t': 15.0, 'value': 0.0, 'ease': 'ease-in-out'}]})
tracks.append(hold_track(DUR_A))
shot_a = {'durationS': DUR_A, 'fps': 30, 'tracks': tracks,
          'camera': [{'startS': 0, 'endS': DUR_A, 'kind': {'type': 'Turntable', 'degrees': 150, 'elevationDeg': 18}}]}
json.dump(shot_a, open(f'{out}/shot_assemble.json', 'w'))

# ---- shot 2: cut to the legs ----------------------------------------------
DUR_C = 7.0
upper = [n for n in links if n.startswith(('sp.', 'sr.', 'ep.', 'ey.')) or n in ('hyaw', 'hpitch')]  # arms + head
drop, tracks = [], []
for name in upper:
    drop += link_instances(name)
# name-based catch for parts the link list files elsewhere (covers on arms, the node head)
for i in doc['instances']:
    pid = i['partDefId']
    if (pid.endswith(('_la', '_ra')) or pid.startswith(('node-', 'head-', 'neck-', 'rs00-head'))
            or pid in ('cover-headcheek', 'cover-headhub', 'cover-neck', 'cover-necksleeve',
                       'trim-neckring', 'trim-neckpuck_l', 'trim-neckpuck_r')) and i['id'] not in drop:
        drop.append(i['id'])
for j, inst in enumerate(drop):
    t = 1.5 + 2.5 * j / max(1, len(drop) - 1)
    tracks.append({'target': {'type': 'Visibility', 'instanceId': inst},
                   'keys': [{'t': 0, 'value': 1}, {'t': t, 'value': 0, 'ease': 'step'}]})
tracks.append(hold_track(DUR_C))
shot_c = {'durationS': DUR_C, 'fps': 30, 'tracks': tracks,
          'camera': [{'startS': 0, 'endS': DUR_C, 'kind': {'type': 'Orbit', 'from': [-55, 16], 'to': [-35, 16]}}]}
json.dump(shot_c, open(f'{out}/shot_cut.json', 'w'))

# ---- shot 3: the fall -------------------------------------------------------
TORSO_Z = links['torso']['origin_mm'][2]  # 587.153: chest origin above the soles
fall = copy.deepcopy(doc)
nodes = fall['nodes']
def add_part(pid, nid, size):
    nodes[str(nid)] = {'id': nid, 'name': pid, 'op': {'type': 'Cube', 'size': {'x': size[0], 'y': size[1], 'z': size[2]}}}
    fall['partDefs'][pid] = {'id': pid, 'name': pid, 'root': nid, 'defaultMaterial': 'default'}
add_part('rig_world', 990001, (0.01, 0.01, 0.01))
add_part('rig_x', 990002, (0.01, 0.01, 0.01))
add_part('rig_z', 990003, (0.01, 0.01, 0.01))
add_part('floor', 990004, (1500.0, 560.0, 6.0))
fall['instances'] = [{'id': f'{p}_i', 'partDefId': p, 'name': p} for p in ('rig_world', 'rig_x', 'rig_z', 'floor')] + fall['instances']
zero = {'x': 0.0, 'y': 0.0, 'z': 0.0}
fall['joints'] = [
    {'id': 'rig_floor', 'name': 'rig_floor', 'parentInstanceId': 'rig_world_i', 'childInstanceId': 'floor_i',
     'parentAnchor': {'x': -1250.0, 'y': -280.0, 'z': -6.0}, 'childAnchor': zero, 'kind': {'type': 'Fixed'}, 'state': 0.0},
    {'id': 'rig_x', 'name': 'rig_x', 'parentInstanceId': 'rig_world_i', 'childInstanceId': 'rig_x_i',
     'parentAnchor': zero, 'childAnchor': zero, 'kind': {'type': 'Slider', 'axis': {'x': 1.0, 'y': 0.0, 'z': 0.0}}, 'state': 0.0},
    {'id': 'rig_z', 'name': 'rig_z', 'parentInstanceId': 'rig_x_i', 'childInstanceId': 'rig_z_i',
     'parentAnchor': zero, 'childAnchor': zero, 'kind': {'type': 'Slider', 'axis': {'x': 0.0, 'y': 0.0, 'z': 1.0}}, 'state': 0.0},
    {'id': 'rig_pitch', 'name': 'rig_pitch', 'parentInstanceId': 'rig_z_i', 'childInstanceId': doc['groundInstanceId'],
     'parentAnchor': {'x': 0.0, 'y': 0.0, 'z': TORSO_Z}, 'childAnchor': zero,
     'kind': {'type': 'Revolute', 'axis': {'x': 0.0, 'y': 1.0, 'z': 0.0}}, 'state': 0.0},
] + fall['joints']
fall['groundInstanceId'] = 'rig_world_i'
json.dump(fall, open(f'{out}/rose-fall.vcad', 'w'))

rows = list(csv.DictReader(open(trace_path)))
HOLD, SLOW = 1.0, 4.0
kx, kz, kp = [{'t': 0, 'value': 0}], [{'t': 0, 'value': 0}], [{'t': 0, 'value': 0}]
last_t = 0.0
for r in rows:
    ts = float(r['t_s'])
    if ts > 1.0:
        break
    tv = HOLD + SLOW * ts
    kx.append({'t': tv, 'value': float(r['base_x_mm'])})
    kz.append({'t': tv, 'value': float(r['base_z_mm']) - TORSO_Z})
    # the trace logs unsigned tilt; this fall goes backward (-x), i.e. negative pitch about +y
    kp.append({'t': tv, 'value': -float(r['tilt_deg'])})
    last_t = tv
for k in (kx, kz, kp):
    k.insert(1, {'t': HOLD, 'value': 0})
DUR_F = last_t + 1.0
shot_f = {'durationS': DUR_F, 'fps': 30, 'tracks': [
    {'target': {'type': 'Joint', 'jointId': 'rig_x'}, 'keys': kx},
    {'target': {'type': 'Joint', 'jointId': 'rig_z'}, 'keys': kz},
    {'target': {'type': 'Joint', 'jointId': 'rig_pitch'}, 'keys': kp},
], 'camera': [{'startS': 0, 'endS': DUR_F, 'kind': {'type': 'Static', 'direction': {'x': 0.35, 'y': -1.0, 'z': 0.32}}}]}
json.dump(shot_f, open(f'{out}/shot_fall.json', 'w'))

print(f'assemble: {len(order)} links, {len(seen)} listed + {len(leftover)} leftover instances')
print(f'cut: {len(drop)} instances drop out')
print(f'fall: {len(kx) - 2} keys, {DUR_F:.2f} s (hold {HOLD} s, {SLOW}x slow), torso z {TORSO_Z}')
