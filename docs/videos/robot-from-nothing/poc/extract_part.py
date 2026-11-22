#!/usr/bin/env python3
"""Pull one partDef out of an exported assembly into a single-root .vcad (for FEA / export).

  python3 extract_part.py rose-mini.vcad shin-outer_ll shin.vcad
"""
import json, sys
src, name, dst = sys.argv[1:4]
d = json.load(open(src))
pd = d['partDefs'][name]
out = {
    'version': d['version'], 'nodes': d['nodes'], 'materials': d['materials'],
    'part_materials': {}, 'roots': [{'root': pd['root'], 'material': pd.get('defaultMaterial', 'default')}],
}
json.dump(out, open(dst, 'w'))
print(f'{name}: root {pd["root"]} material {pd.get("defaultMaterial")} -> {dst}')
