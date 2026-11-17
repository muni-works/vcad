#!/usr/bin/env python3
"""Find a constant beat grid for the soundtrack: onset envelope → tempo →
phase that maximises on-grid onset energy. Writes beat.json {bpm, phase} and
prints per-bar loudness (to place drops) plus a drift check (the best phase
fitted separately per 20 s window; a constant grid should agree everywhere).

    python3 beat.py track.mp3 [beat.json]
"""
import json
import subprocess
import sys

import numpy as np

SR, HOP = 22050, 256


def decode(path):
    raw = subprocess.run(["ffmpeg", "-loglevel", "error", "-i", path, "-ac", "1",
                          "-ar", str(SR), "-f", "f32le", "-"],
                         check=True, capture_output=True).stdout
    return np.frombuffer(raw, dtype=np.float32)


def onset_envelope(x):
    n = 1024
    win = np.hanning(n).astype(np.float32)
    frames = np.lib.stride_tricks.sliding_window_view(x, n)[::HOP] * win
    mag = np.log1p(30 * np.abs(np.fft.rfft(frames, axis=1)))
    flux = np.maximum(np.diff(mag, axis=0), 0).sum(axis=1)
    flux = flux - np.convolve(flux, np.ones(16) / 16, mode="same")
    return np.maximum(flux, 0), np.sqrt((frames ** 2).mean(axis=1))


def grid_score(env, bpm, phase, t0=0.0, t1=None):
    fr = SR / HOP
    t1 = len(env) / fr if t1 is None else t1
    period = 60 / bpm
    k0 = int(np.ceil((t0 - phase) / period))
    ts = phase + period * np.arange(k0, int((t1 - phase) / period) + 1)
    idx = np.round(ts * fr).astype(int)
    idx = idx[(idx >= 1) & (idx < len(env) - 1)]
    return (env[idx] + 0.5 * env[idx - 1] + 0.5 * env[idx + 1]).mean()


def main():
    path = sys.argv[1]
    out = sys.argv[2] if len(sys.argv) > 2 else "beat.json"
    x = decode(path)
    env, rms = onset_envelope(x)
    fr = SR / HOP
    # tempo: autocorrelation peak in 70–180 BPM, refined on a fine grid by grid score
    ac = np.correlate(env - env.mean(), env - env.mean(), mode="full")[len(env) - 1:]
    lags = np.arange(len(ac)) / fr
    ok = (lags > 60 / 180) & (lags < 60 / 70)
    lag = lags[ok][np.argmax(ac[ok])]
    bpm0 = 60 / lag
    best = (0, None, None)
    for bpm in np.arange(bpm0 - 2, bpm0 + 2, 0.01):
        period = 60 / bpm
        for phase in np.arange(0, period, 0.005):
            s = grid_score(env, bpm, phase)
            if s > best[0]:
                best = (s, bpm, phase)
    score, bpm, phase = best
    off = grid_score(env, bpm, phase + 30 / bpm)  # half a beat off
    print(f"autocorr {bpm0:.2f} BPM → grid {bpm:.2f} BPM, phase {phase:.3f} s; "
          f"on-grid / off-beat onset energy {score / off:.2f}×")
    # drift: best phase per 20 s window, as an offset from the global grid
    period = 60 / bpm
    dur = len(x) / SR
    for w0 in np.arange(0, dur - 10, 20):
        cands = np.arange(-0.08, 0.081, 0.005)
        s = [grid_score(env, bpm, phase + c, w0, w0 + 20) for c in cands]
        print(f"  window {w0:5.0f}-{w0 + 20:5.0f} s: best offset {cands[int(np.argmax(s))] * 1000:+5.0f} ms")
    # loudness per bar (4 beats), to place drops
    print("bar  t(s)   rms")
    bar = 4 * period
    n_bars = int((dur - phase) / bar)
    for b in range(n_bars):
        a, z = int((phase + b * bar) * fr), int((phase + (b + 1) * bar) * fr)
        r = rms[a:z].mean() if z > a else 0
        print(f"{b:3d} {phase + b * bar:6.2f} {r:.3f} {'#' * int(r * 200)}")
    json.dump({"bpm": round(float(bpm), 3), "phase": round(float(phase), 3)}, open(out, "w"))


if __name__ == "__main__":
    main()
