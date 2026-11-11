# M4: photo-measure loop (print → photograph → compare → compensate)

Status: design note. Wire up once image-to-CAD (#1081) lands; that PR is
blocked on irys-tags reaching irys main, so nothing here depends on it yet.

## Goal

Close the loop between the model and the part that actually came off the
machine. An agent prints (or mills) a part, the user photographs it, vcad
measures the photo against the model, and the next job is compensated. Today
the loop ends at "G-code verified". This adds "part verified".

## The loop, as tasks

All steps run as MCP Tasks (`crates/vcad-atoms`), with the same fail-closed
rule as fab: a step that can't measure honestly fails. It never returns a
guess.

1. **`fab_print` / `fab_mill`** (exists, dry-run). Its result carries the job
   id, the part pose on the bed or stock, and the exact document revision.
2. **`capture_part`**: elicitation asks the user to photograph the part. The
   request is shaped: top-down on the calibration card, then one oblique
   shot. The app shows a camera overlay and the agent waits on the
   elicitation. No hardware access is needed beyond the Mac camera or an
   imported photo.
3. **`measure_photo`**: image-to-CAD (#1081) segments the part's silhouette
   and edges. A printed ArUco/ChArUco **calibration card** in frame gives
   metric scale and lens undistortion. Without a card the step fails closed:
   scale from "known part size" is circular, because the size is what's being
   measured.
4. **`compare_to_model`**: register the measured silhouette or edges to the
   model's projection at the same pose (ICP over 2D contours, seeded by the
   card frame). Emit per-feature deviations: hole diameters, outline offset,
   slot widths. Each deviation gets an uncertainty band from the card's
   reprojection residual and the pixel scale.
5. **`compensate`**: map deviations to corrections.
   - Printers: XY hole/outline compensation (Bambu "X-Y hole/contour
     compensation" equivalents), elephant-foot, and shrink scale per
     material. These are written to a per-printer, per-material **calibration
     profile**, not baked into the model.
   - Mills: tool-diameter wear offset and deflection allowance in the CAM
     tool library.
   - Optionally, propose a document-parameter change through
     `optimize_parameters`, using the deviation as a target ("hole measures
     4.82; drive `hole_d` until the *predicted printed* size is 5.00"). The
     gradient seam already prices `d(geometry)/d(parameter)`.
6. **Re-run fab** with the profile applied. The receipt records which
   calibration profile and which measurement produced it.

## Receipts and honesty

- Measurements are `vcad-receipt` claims with basis `measured`, plus the photo
  hash, card id and residual. An edit to the document makes them **Stale**,
  same as M2 badges.
- The claim is Provisional when the uncertainty band exceeds the tolerance
  being claimed. It is Pass only when the band fits inside the tolerance.
- A compensation is only applied when the deviation is larger than its
  uncertainty. Noise should not cause the system to chase it.
- A photo can't see Z steps, internal features or tolerances below roughly
  2 px at the card's scale. The tool reports what it cannot measure and does
  not imply it measured it.

## Interfaces to settle when #1081 lands

- Image-to-CAD output type. We need metric 2D contours plus edge confidence,
  not just a reconstructed solid.
- Where calibration profiles live: `~/.vcad/calibration/<machine>/<material>.json`,
  local only, no sync (consistent with "no hosted services").
- Elicitation UI for the capture step (M2 owns elicitation; coordinate the
  camera overlay with M1's viewport).

## Safety

Compensation never dispatches anything. It only changes the next job's
inputs, which go back through the same verify gates. Machine sends stay
dry-run until real dispatch is wired behind the Swift app's existing gates
and a human confirm.
