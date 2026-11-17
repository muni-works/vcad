#!/usr/bin/env python3
"""Voice the narration with ribt and time every word.

    python voice.py <ribt-tts binary> <weights dir> <out dir> [line id …]

With line ids, only those lines are re-voiced; the rest of timing.json is kept.

For each line in narration.py: `ribt-tts synth` (the released settings,
release.toml) → <out>/<id>.wav, then Whisper word timestamps, matched back
onto the script's own words (difflib; unmatched words are interpolated).
Writes <out>/timing.json: {id: {"dur": s, "words": [[word, start, end], …]}}.
Needs openai-whisper (run with a Python that has it).
"""
import difflib
import json
import os
import re
import subprocess
import sys
import tempfile
import wave

from narration import LINES


def norm(w):
    return re.sub(r"[^a-z0-9']", "", w.lower())


def synth(tts, weights, text, out_wav):
    with tempfile.TemporaryDirectory() as tmp:
        r = subprocess.run(
            [tts, "synth", "--run", weights, "--ckpt", "acoustic_af_bon.safetensors",
             "--flow", os.path.join(weights, "flow_student_af.safetensors"), "--nfe", "1", "--shortcut",
             "--temp", "0.3", "--voc", os.path.join(weights, "vocoder_vocos_import.safetensors"),
             "--ref", os.path.join(weights, "ref_afheart.wav"), "--seed", "0", "--pitch", "1",
             "--accent", "minnesota", "--text", text, "--out-dir", tmp],
            capture_output=True, text=True)
        for ln in (r.stdout + r.stderr).splitlines():
            if "OOV" in ln or "rror" in ln:
                print("   ", ln)
        # ribt writes float WAVE_EXTENSIBLE tagged left-only: re-tag as 16-bit mono
        subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-i", os.path.join(tmp, "out_ref.wav"),
                        "-ac", "1", "-c:a", "pcm_s16le", out_wav], check=True)
    with wave.open(out_wav) as w:
        return w.getnframes() / w.getframerate()


def main():
    tts, weights, out = sys.argv[1:4]
    os.makedirs(out, exist_ok=True)
    import whisper
    model = whisper.load_model("small.en")
    only = sys.argv[4:]
    tpath = os.path.join(out, "timing.json")
    timing = json.load(open(tpath)) if only else {}
    for lid, text in LINES:
        if only and lid not in only:
            continue
        wav = os.path.join(out, f"{lid}.wav")
        dur = synth(tts, weights, text, wav)
        res = model.transcribe(wav, word_timestamps=True, language="en")
        heard = [(norm(w["word"]), w["start"], w["end"]) for s in res["segments"] for w in s["words"]]
        heard = [h for h in heard if h[0]]
        script = text.split()
        sm = difflib.SequenceMatcher(a=[norm(w) for w in script], b=[h[0] for h in heard], autojunk=False)
        t0 = [None] * len(script)
        t1 = [None] * len(script)
        for a, b, n in sm.get_matching_blocks():
            for k in range(n):
                t0[a + k], t1[a + k] = heard[b + k][1], heard[b + k][2]
        # interpolate the words Whisper heard differently
        known = [i for i, v in enumerate(t0) if v is not None]
        for i in range(len(script)):
            if t0[i] is None:
                lo = max([k for k in known if k < i], default=None)
                hi = min([k for k in known if k > i], default=None)
                a = t1[lo] if lo is not None else 0.0
                b = t0[hi] if hi is not None else dur
                n_gap = (hi if hi is not None else len(script)) - (lo if lo is not None else -1)
                j = i - (lo if lo is not None else -1)
                t0[i] = a + (b - a) * (j - 1) / max(n_gap - 1, 1)
                t1[i] = a + (b - a) * j / max(n_gap - 1, 1)
        missed = sum(1 for i in range(len(script)) if i not in known)
        timing[lid] = {"dur": round(dur, 3),
                       "words": [[w, round(a, 3), round(b, 3)] for w, a, b in zip(script, t0, t1)]}
        print(f"{lid:7s} {dur:5.2f} s  {len(script):2d} words, {missed} interpolated  | "
              + " ".join(h[0] for h in heard)[:90])
    timing = {lid: timing[lid] for lid, _ in LINES}
    json.dump(timing, open(tpath, "w"), indent=1)
    print("total speech %.1f s" % sum(v["dur"] for v in timing.values()))


if __name__ == "__main__":
    main()
