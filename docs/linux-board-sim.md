# Linux-board sim: pinned gateware artifacts

vcad simulates a Linux board at the pin level: the SoC gateware runs in
Verilator, and every board net between the FPGA balls and the other parts is
resolved in Rust. vcad never runs the HDL generator. LiteX is Python, so it
runs once, offline, and its output is frozen into a **gateware artifact**: a
tarball named by its sha256.

## Artifact format (`vcad.gateware/1`)

```text
gateware/manifest.json   schema, top module, clock port + Hz, pin map, CPU sha, provenance
gateware/rtl/            Verilog + $readmemh init files (the model runs with cwd = rtl/)
gateware/csr.json        LiteX address map
gateware/images/         kernel Image, opensbi.bin, rootfs.cpio.gz, dtb, boot_ram0.json
gateware/gen/            gen.py (generator), pack.py, Dockerfile it ran in
```

The manifest's `files` map pins the sha256 of every other file.
`vcad_ecad_sim::gateware::Artifact::open` re-hashes all of them and fails
closed on any mismatch, missing file, or unlisted file. The tarball's own sha
is checked before it is extracted. The tarball is deterministic
(`tar --sort=name --mtime=@0 --owner=0 --group=0 | gzip -n`).

`ArtifactStore` caches extracted artifacts in `$VCAD_GATEWARE_DIR` (default
`~/.vcad/gateware/<sha>/`). Missing ones are fetched from the URL template
`$VCAD_GATEWARE_URL` (`{sha}`, `{sha12}`). The default template is
`https://github.com/muni-works/vcad/releases/download/gateware-{sha12}/gateware-{sha}.tar.gz`.
To use an artifact offline, drop `gateware-<sha>.tar.gz` into the store
directory.

## M1 artifact

`63d8d63f05f42bc15655e9ee1bf070b3d2b0f47c6c9e4b30642accc0d6c7df93`
(`gateware::ULX3S_UART_LINUX`, 12 MB)

- SoC: linux-on-litex-vexriscv with VexRiscv-SMP (1 core, rv32ima, sv32) at a
  nominal 100 MHz. The console UART is LiteX's `RS232PHY` on top-level pads.
  The pin map comes from litex-boards `radiona_ulx3s`: ball L4 is `serial.tx`
  (out) and ball M1 is `serial.rx` (in).
- SDRAM: LiteDRAM controller plus `SDRAMPHYModel`, which works at the DFI
  level and is not yet pin-level (that is M2). The boot images are baked into
  its init files.
- CPU netlist: `…_Ldw32_Ood.v`, sha256 `0bba8e6a…`. This is the configuration
  the linux-board spike's full pin-level boot ran, and M1 pins it so results
  can be compared with that oracle. It is **not** the ULX3S bitstream-matched
  `…_Ood_Wm.v` (`9ab07065…`, built with `--with-wishbone-memory`). Switching
  to the matched netlist means regenerating the artifact.
- Images: `linux_2022_03_23.zip` from
  linux-on-litex-vexriscv issue #164 (sha256 `9ad7a043…fab48`).
- Generator: `gen/gen.py` at litex `cea96b5`, migen `4c2ae8d`, litedram
  `60b5ae1`, litex-boards `c710663`, pythondata-cpu-vexriscv-smp `0b753ec`,
  linux-on-litex-vexriscv `df09691`. It ran in an Ubuntu 24.04 container with
  OSS CAD Suite 2026-11-16 (all commits are recorded in the manifest).

### Regenerating

Run this inside the generator container (`gen/Dockerfile`, with the LiteX
repos at the commits listed above):

```bash
cd linux-on-litex-vexriscv && python3 gen.py OUT && python3 pack.py
```

The output is a new tarball with a new sha. Pin it in
`gateware::ULX3S_UART_LINUX`. No Python runs in the vcad tree or in CI.
