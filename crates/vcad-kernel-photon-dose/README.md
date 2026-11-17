# vcad-kernel-photon-dose

Photon (x-ray and gamma) shielding dose for radiation-producing benches:
thick-target bremsstrahlung and neutron-capture gamma sources,
layered-slab photon Monte Carlo with computed buildup, ICRP-74 H*(10) and
air kerma. Every number comes with a Monte Carlo error and a named
systematic band. Emits `vcad.photon-claims/1` predicted claims and sums
neutron + photon dose with `vcad-kernel-neutronics`.

- NIST SRD 126 attenuation data (5 keV – 3 MeV), Klein–Nishina scatter.
- Kramers thick-target source, 10–150 kV validity band, enforced.
- IEC-chamber model: wall path + viewport path (`spec::ChamberSpec`).
- Capture gammas (H 2.22 MeV, ¹⁰B 478 keV) from a neutronics run.
- Fail-closed throughout: unknown materials, out-of-band voltages, soft
  leaks past the 10 keV dose floor, truncated or unscored runs.

```bash
cargo run --release -p vcad-kernel-photon-dose --example fusor_xray
```

Design notes, validation tables and the fusor numbers:
[`docs/photon-dose-m0.md`](../../docs/photon-dose-m0.md).

Part of [vcad](https://github.com/ecto/vcad).
