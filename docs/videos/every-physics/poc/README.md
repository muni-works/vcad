# every-physics PoC

one loon document ([`../puck.loon`](../puck.loon)) → evaluated once → fed to
the diff seam, DFM, modal FEA, acoustics, thermal and antenna. each printed
number is tagged `[doc-geom]` (read off the evaluated solid), `[doc-param]`
(read off `doc.parameters`, hand-mapped into a spec by this driver; this is the
missing adapter, prototyped) or `[assumed]`.

standalone package, not a workspace member (empty `[workspace]`; mirrors the
root `[patch.crates-io]`). build on a build box, never the Mac:

```bash
cd docs/videos/every-physics/poc
cp ../../../../Cargo.lock .
MODAL_QUALITY=preview cargo run --release -- ../puck.loon out
python3 plot.py out
```

`MODAL_QUALITY=fine|preview|skip` (default fine; fine didn't finish in ~25 min
on mew). `results/` holds the committed results (copied from `out/`, which is gitignored) of the preview run, plus the two
renders (`vcad-render out/puck.vcad --view hero`, and `--section y=0` with
the air root removed).
