#!/usr/bin/env python3
"""Score for the puck video, synthesized from the puck's own predictions (numpy + scipy).

usage: synth.py out.wav

- tuning: Eb3 = 155.0 Hz, the field-solved port tuning (so A4 = 438.4 Hz).
- drone: 77.5 Hz, the port tuning an octave down.
- bell: the six free-free shell modes from the Fine modal solve (448.1, 459.0,
  545.4, 630.4, 672.3, 886.8 Hz), struck together, i.e. roughly what the puck
  sounds like when you hit it. Inharmonic against the key on purpose.
- the optimizer bars glide a tone along the optimizer's own f_b trajectory.
96 BPM, 20 bars = 50 s. Timeline must match comp.py."""
import sys, wave
import numpy as np
from scipy.signal import lfilter

SR, BPM = 44100, 96.0
BEAT = 60 / BPM; BAR = 4 * BEAT
DUR = 20 * BAR
N = int(SR * DUR)
t = np.arange(N) / SR
rng = np.random.default_rng(11)
L = np.zeros(N); R = np.zeros(N)

F_PORT = 155.0
MODES = [448.1, 459.0, 545.4, 630.4, 672.3, 886.8]
OPT_FB = [140.5, 110.9, 109.2, 107.7, 107.3, 106.8, 106.5, 107.2, 129.0, 125.1, 125.8, 126.2, 126.2, 126.3, 128.6]

def hz(m): return F_PORT * 2 ** ((m - 51) / 12)   # Eb3 (midi 51) = port tuning
def lp(x, fc, blk=512):
    """one-pole low-pass, cutoff piecewise-constant per block (vectorized)."""
    fc = np.broadcast_to(np.asarray(fc, float), x.shape)
    y = np.empty_like(x); z = np.zeros(1)
    for i in range(0, len(x), blk):
        a = 1 - np.exp(-2 * np.pi * fc[i] / SR)
        y[i:i + blk], z = lfilter([a], [1, a - 1], x[i:i + blk], zi=z)
    return y
def saw(f, tt, ph=0.0): return 2 * ((f * tt + ph) % 1.0) - 1
def env_ar(n, a, r):
    e = np.ones(n); a = int(a * SR); r = int(r * SR)
    if a: e[:a] = np.linspace(0, 1, a)
    if r: e[-r:] *= np.linspace(1, 0, r)
    return e
def put(sig, start, pan=0.0, gain=1.0):
    i = int(start * SR)
    if i >= N: return
    j = min(N, i + len(sig)); s = sig[: j - i] * gain
    L[i:j] += s * np.sqrt((1 - pan) / 2); R[i:j] += s * np.sqrt((1 + pan) / 2)
def b(bar, beat=0.0): return bar * BAR + beat * BEAT

# ---- drone: the port tuning, an octave down, breathing
dr = sum(np.sin(2 * np.pi * (F_PORT / 2) * (1 + d) * t + p) for d, p in ((0, 0), (0.003, 1), (-0.002, 2)))
dr += 0.3 * saw(F_PORT / 2, t)
dr = lp(dr, 180 + 220 * (0.5 + 0.5 * np.sin(2 * np.pi * t / 7.5)))
dr *= np.clip(t / 4, 0, 1) * np.clip((DUR - t) / 3, 0, 1) * 0.11
put(dr, 0)

# ---- pads: Ebm | Cb | Gb | Bb, one chord per bar from bar 2
CH = [(51, 54, 58, 63), (47, 51, 54, 59), (42, 49, 54, 58), (46, 50, 53, 58)]
for bar in range(1, 20):
    notes = CH[bar % 4]; n = int((BAR + 1.2) * SR); tt = np.arange(n) / SR
    sig = sum(saw(hz(m) * (1 + (k - 1) * 0.004), tt, rng.random()) for m in notes for k in range(3))
    open_ = 700 + 900 * np.clip((bar - 4) / 10, 0, 1)
    sig = lp(sig, open_ + 500 * np.sin(np.pi * np.clip(tt / BAR, 0, 1)))
    g = 0.012 if bar < 4 else 0.016
    sig *= env_ar(n, 0.9 if bar < 4 else 0.25, 1.2) * g
    put(sig, b(bar), pan=-0.35); put(np.roll(sig, 700), b(bar), pan=0.35)

# ---- the puck, struck: six modes, each its own decay
def bell(gain=1.0, dur=4.0):
    n = int(dur * SR); tt = np.arange(n) / SR
    amps = [1.0, 0.8, 0.55, 0.45, 0.35, 0.22]
    s = sum(a * np.sin(2 * np.pi * f * tt) * np.exp(-tt * (1.1 + 0.6 * k)) for k, (f, a) in enumerate(zip(MODES, amps)))
    click = lp(rng.standard_normal(n) * np.exp(-tt * 90), 4000) * 0.4
    return (s + click) * gain * 0.09
BELL_BARS = [2, 3, 4] + list(range(6, 15)) + [17, 19]
for bar in BELL_BARS:
    g = 1.6 if bar == 9 else (1.2 if bar in (4, 17) else 0.8)   # bar 9 = the modal card
    put(bell(g), b(bar), pan=0.15 * (-1) ** bar)

# ---- drums from bar 4 (the section reveal)
def kick():
    n = int(0.45 * SR); tt = np.arange(n) / SR
    f = 45 + 95 * np.exp(-tt * 28); ph = 2 * np.pi * np.cumsum(f) / SR
    return np.sin(ph) * np.exp(-tt * 7) * 0.5
def hat(open_=False):
    n = int((0.25 if open_ else 0.06) * SR); tt = np.arange(n) / SR
    x = rng.standard_normal(n); x = x - lp(x, 6000)
    return x * np.exp(-tt * (14 if open_ else 70)) * 0.06
def clap():
    n = int(0.3 * SR); tt = np.arange(n) / SR
    x = rng.standard_normal(n); x = lp(x - lp(x, 900), 3500)
    e = sum(np.exp(-np.clip(tt - d, 0, None) * 60) * (tt >= d) for d in (0, 0.011, 0.023)) + np.exp(-tt * 12) * 0.5
    return x * e * 0.16
K, Hc, Ho, C = kick(), hat(), hat(True), clap()
for bar in range(4, 20):
    if bar in (15, 16) or bar >= 18: continue          # optimizer break + outro
    for bt in range(4):
        put(K, b(bar, bt), gain=1.0 if bar >= 6 else 0.7)
        put(Hc, b(bar, bt + 0.5), pan=0.3)
        if bar >= 6 and bt in (1, 3): put(C, b(bar, bt), pan=-0.1)
    if bar >= 6: put(Ho, b(bar, 3.5), pan=0.4)

# ---- arp, 16ths, bars 6-14 (the solver cards)
step = BEAT / 4
for bar in range(6, 15):
    notes = CH[bar % 4]
    for i in range(16):
        m = notes[[0, 2, 1, 3][i % 4]] + 12 + (12 if i in (6, 14) else 0)
        n = int(0.2 * SR); tt = np.arange(n) / SR
        s = np.sign(np.sin(2 * np.pi * hz(m) * tt)) * 0.4 + np.sin(2 * np.pi * hz(m) * tt)
        s = lp(s, 2400 * np.exp(-tt * 16) + 300) * np.exp(-tt * 20) * 0.045
        put(s, b(bar) + i * step, pan=0.45 if i % 2 else -0.45)
        put(s * 0.3, b(bar) + (i + 3) * step, pan=-0.45 if i % 2 else 0.45)

# ---- optimizer bars 15-17: a tone walks the optimizer's own f_b history
t0, t1 = b(15), b(17)
n = int((t1 - t0 + 1.0) * SR); tt = np.arange(n) / SR
pos = np.clip(tt / (t1 - t0), 0, 1) * (len(OPT_FB) - 1)
f = np.interp(pos, np.arange(len(OPT_FB)), OPT_FB) * 2   # an octave up so it reads
ph = 2 * np.pi * np.cumsum(f) / SR
tone = (np.sin(ph) + 0.35 * saw(1, ph / (2 * np.pi))) * env_ar(n, 0.3, 1.0) * 0.07
put(lp(tone, 1500), t0)
riser = lp(rng.standard_normal(int(BAR * 2 * SR)), np.linspace(300, 7000, int(BAR * 2 * SR))) * np.linspace(0, 1, int(BAR * 2 * SR)) ** 2 * 0.05
put(riser, b(15))

# ---- master: soft clip, normalize, fades
mix = np.stack([L, R], 1)
mix = np.tanh(mix * 1.6) / np.tanh(1.6)
mix /= np.max(np.abs(mix)) + 1e-9
mix *= 0.89 * np.clip(t / 0.05, 0, 1)[:, None] * np.clip((DUR - t) / 2.5, 0, 1)[:, None]
with wave.open(sys.argv[1], "wb") as w:
    w.setnchannels(2); w.setsampwidth(2); w.setframerate(SR)
    w.writeframes((mix * 32767).astype("<i2").tobytes())
print(f"{sys.argv[1]}: {DUR:.2f} s, 96 BPM, Eb3 = {F_PORT} Hz")
