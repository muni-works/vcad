# STEP interop corpus + scorecard

21 public STEP files from Fusion 360, Onshape, SolidWorks, FreeCAD and KiCad
(`manifest.tsv`: source URL pinned to a commit, claimed origin, licence). The
files are fetched, not vendored — several are tens of MB and GPL /
CC-BY-NC-SA / CERN-OHL.

```bash
scripts/step-corpus/fetch.sh                      # → ~/.cache/vcad-step-corpus, sha256-verified
cargo build --release -p vcad-kernel-tessellate --example step_scorecard
python3 -m venv ~/venvs/occt && ~/venvs/occt/bin/pip install cadquery-ocp   # optional OCCT reference
python3 scripts/step-corpus/scorecard.py --occt ~/venvs/occt/bin/python \
    --compare scripts/step-corpus/results/before.json --out after.json
```

Per file (each in its own process under `--timeout`, so a hang is a `HANG`
row): import success and time, faces vs OCCT (with assembly occurrences
placed), closed solids (0 open / 0 over-used mesh edges at 32 segments),
placed volume vs OCCT, colours kept, assembly instances, and a round trip
(vcad export → vcad re-import faces; and OCCT opening vcad's export, with
`BRepCheck_Analyzer` validity: `rt-occt`).

`results/before.json` is `main` at b82b0a58a; `results/after.json` is this
branch. Volume on a mesh that isn't closed is reported but meaningless.
