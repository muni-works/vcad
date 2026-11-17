#!/usr/bin/env python3
"""Build-up and cut-down states for the teaser, as one still .vcad per state.

  python3 make_stills.py rose-mini.vcad rose-mini.joints.json outdir/

`vcad-render --animate` poses joints only: Visibility / Explode tracks and the
camera list are accepted and silently ignored. So the two shots that need parts
to appear or vanish are rendered as stills, one per state, and crossfaded in
comp.py. A hidden instance keeps its id and joints (so FK is unchanged) but
points at a 0.01 mm cube; two such cubes pinned at the full robot's bounds keep
the fit, and therefore the framing, identical across every state.

writes outdir/assemble_NN.vcad (parts arrive link by link, feet up) and
outdir/cut_NN.vcad (arms and head drop out, leaving rml-12's legs).
"""
import copy, json, sys

doc_path, joints_path, out = sys.argv[1:4]
doc = json.load(open(doc_path))
links = json.load(open(joints_path))['links']
part_to_inst = {i['partDefId']: i['id'] for i in doc['instances']}

NID = 990100
doc['nodes'][str(NID)] = {'id': NID, 'name': 'nothing', 'op': {'type': 'Cube', 'size': {'x': 0.01, 'y': 0.01, 'z': 0.01}}}
doc['partDefs']['nothing'] = {'id': 'nothing', 'name': 'nothing', 'root': NID, 'defaultMaterial': 'default'}
bs = [l['aabb_mm'] for l in links.values()]
lo = [min(b[0][k] for b in bs) - 5 for k in range(3)]
hi = [max(b[1][k] for b in bs) + 5 for k in range(3)]
for name, p in (('anchor_lo', lo), ('anchor_hi', hi)):
    doc['instances'].append({'id': name, 'partDefId': 'nothing', 'name': name, 'transform': {
        'translation': dict(zip('xyz', p)), 'rotation': {'x': 0.0, 'y': 0.0, 'z': 0.0}, 'scale': {'x': 1.0, 'y': 1.0, 'z': 1.0}}})


def link_instances(name):
    names = [p['name'] if isinstance(p, dict) else p for p in links[name].get('parts', [])]
    return [part_to_inst[n] for n in names if n in part_to_inst]


def write(path, visible):
    d = copy.deepcopy(doc)
    for i in d['instances']:
        if i['id'] not in visible and not i['id'].startswith('anchor_'):
            i['partDefId'] = 'nothing'
    json.dump(d, open(path, 'w'))


all_ids = {i['id'] for i in doc['instances']}

# assemble: cumulative, one state per link, lowest link first
order = sorted(links, key=lambda n: links[n]['aabb_mm'][0][2])
shown = set()
for k, name in enumerate(order):
    shown |= set(link_instances(name))
    write(f'{out}/assemble_{k:02d}.vcad', all_ids if k == len(order) - 1 else shown)

# cut: drop the upper body in four bites
upper = [n for n in links if n.startswith(('sp.', 'sr.', 'ep.', 'ey.')) or n in ('hyaw', 'hpitch')]
drop = [i for n in upper for i in link_instances(n)]
for i in doc['instances']:
    pid = i['partDefId']
    if (pid.endswith(('_la', '_ra')) or pid.startswith(('node-', 'head-', 'neck-', 'rs00-head'))
            or pid in ('cover-headcheek', 'cover-headhub', 'cover-neck', 'cover-necksleeve',
                       'trim-neckring', 'trim-neckpuck_l', 'trim-neckpuck_r')) and i['id'] not in drop:
        drop.append(i['id'])
BITES = 4
for k in range(BITES + 1):
    gone = set(drop[: len(drop) * k // BITES])
    write(f'{out}/cut_{k:02d}.vcad', all_ids - gone)
print(f'assemble: {len(order)} states; cut: {BITES + 1} states, {len(drop)} instances drop')
