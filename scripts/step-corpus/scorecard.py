#!/usr/bin/env python3
"""Re-run the STEP interop scorecard over the corpus.

    scripts/step-corpus/fetch.sh
    cargo build --release -p vcad-kernel-tessellate --example step_scorecard
    python3 scripts/step-corpus/scorecard.py [--occt /path/to/venv/bin/python] \
        [--out scorecard.json] [--compare before.json] [--timeout 120]

Each file runs in its own process under a timeout, so a hang is a TIMEOUT
row, not a stuck run. With --occt, OCCT (cadquery-ocp) provides the
reference face/solid/volume/colour counts and also opens vcad's round-trip
export (rt_occt_*): "another tool can read what we wrote".
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "../.."))


def run_json(cmd, timeout):
    t = time.time()
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return {"timeout": True, "wall_s": timeout}
    lines = [l for l in p.stdout.splitlines() if l.startswith("{")]
    if not lines:
        return {"crash": (p.stderr.strip().splitlines() or ["?"])[-1][:160],
                "wall_s": round(time.time() - t, 2)}
    d = json.loads(lines[-1])
    d["wall_s"] = round(time.time() - t, 2)
    return d


def verdict(r):
    if r.get("timeout"):
        return "HANG"
    if "crash" in r:
        return "CRASH"
    if not r.get("read_ok"):
        return "FAIL"
    bad = []
    if r.get("skipped_faces"):
        bad.append("skipped")
    if r.get("solids", 0) and r.get("closed_solids", 0) < r["solids"]:
        bad.append("open")
    ov = r.get("occt_volume")
    if ov and abs(r.get("volume", 0) - ov) > 0.02 * abs(ov):
        bad.append("vol")
    placed = r.get("placed_faces", r.get("faces"))
    if r.get("occt_faces") and placed != r["occt_faces"]:
        bad.append("faces")
    if r.get("occt_colored_shapes") and not r.get("colored_solids"):
        bad.append("colour")
    if r.get("rt_error") or (r.get("rt_faces") is not None and r["rt_faces"] != r.get("faces")):
        bad.append("rt")
    if r.get("rt_occt_ok") is False or r.get("rt_occt_valid") is False:
        bad.append("rt-occt")
    return "OK" if not bad else ",".join(bad)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default=os.environ.get(
        "STEP_CORPUS_DIR", os.path.expanduser("~/.cache/vcad-step-corpus")))
    ap.add_argument("--bin", default=os.path.join(
        os.environ.get("CARGO_TARGET_DIR", os.path.join(ROOT, "target")),
        "release/examples/step_scorecard"))
    ap.add_argument("--occt", help="python with cadquery-ocp installed")
    ap.add_argument("--timeout", type=int, default=120)
    ap.add_argument("--out", default="scorecard.json")
    ap.add_argument("--compare", help="earlier scorecard.json for a before/after table")
    ap.add_argument("--only", help="substring filter on file id")
    a = ap.parse_args()

    files = sorted(f for f in os.listdir(a.corpus) if f.lower().endswith((".step", ".stp")))
    if a.only:
        files = [f for f in files if a.only in f]
    rows = {}
    tmp = tempfile.mkdtemp(prefix="step-scorecard-")
    for f in files:
        path = os.path.join(a.corpus, f)
        rt = os.path.join(tmp, f + ".rt.step")
        r = run_json([a.bin, path, rt], a.timeout)
        if a.occt:
            ref = run_json([a.occt, os.path.join(HERE, "occt_ref.py"), path], a.timeout)
            r.update({k: v for k, v in ref.items() if k.startswith("occt_")})
            if os.path.exists(rt):
                back = run_json([a.occt, os.path.join(HERE, "occt_ref.py"), rt], a.timeout)
                r.update({"rt_" + k: v for k, v in back.items() if k.startswith("occt_")})
        r["verdict"] = verdict(r)
        rows[f] = r
        print(f"{f:40s} {r['verdict']:24s} {r.get('wall_s')}s", file=sys.stderr, flush=True)
    json.dump(rows, open(a.out, "w"), indent=1, sort_keys=True)

    before = json.load(open(a.compare)) if a.compare else {}
    cols = ["file", "faces (vcad/OCCT)", "closed", "vol err", "colours", "asm inst", "import s", "verdict"]
    if before:
        cols.insert(1, "before")
    print("| " + " | ".join(cols) + " |")
    print("|" + "---|" * len(cols))
    for f, r in rows.items():
        ov = r.get("occt_volume")
        verr = f"{100 * (r.get('volume', 0) - ov) / ov:+.1f}%" if ov and r.get("read_ok") else "–"
        cells = [
            f.rsplit(".", 1)[0],
            f"{r.get('placed_faces', r.get('faces', '–'))}/{r.get('occt_faces', '?')}",
            f"{r.get('closed_solids', '–')}/{r.get('solids', '–')}",
            verr,
            f"{r.get('colored_solids', 0)}/{r.get('occt_colored_shapes', '?')}",
            f"{r.get('asm_instances', '–')}",
            f"{r.get('wall_s')}",
            r["verdict"],
        ]
        if before:
            b = before.get(f, {})
            cells.insert(1, f"{b.get('verdict', '–')} ({b.get('wall_s', '–')}s)")
        print("| " + " | ".join(str(c) for c in cells) + " |")


if __name__ == "__main__":
    main()
