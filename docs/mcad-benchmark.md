# MCAD benchmark

Ten real parts, each built only through the Mac app by mouse and keyboard. A
part passes when its volume and key dimensions match the hand calculation.
Every friction point found gets fixed or listed here.

| # | Part | Exercises | Status |
|---|------|-----------|--------|
| 1 | NEMA 17 bracket | sketch, extrude, cuts, slots, fillet | Built |
| 2 | Flanged bearing housing | revolve, holes, counterbores, circular pattern | Built: 18,981 mm^3 |
| 3 | Sheet-metal enclosure | flanges, bends, flat pattern | Built: flat 156.4 x 116.4 mm, 15,018 mm^2; folded 22,734 mm^3 |
| 4 | 3D-print snap-fit case | shell, lip, clips, tolerance | Partial: 60 x 40 x 25 box shelled 2 mm, open top, 13,632 mm^3 (hand 13,632). No lip or clips yet |
| 5 | Swept and lofted duct | sweep, loft, shell | Partial: loft from 40 x 30 to 20 x 15 over 50 mm, 35,000 mm^3 (frustum 35,000). Shelled 2 mm with the top open: 11,667.0 mm^3 in the app against 11,653.9 closed form (0.11% high; the kernel test matches to 1e-6, so the gap is unexplained). Sweep: a 3 mm circle swept one helix turn over 50 mm builds a clean tube, 4,338.4 mm^3 against 3,823.6 for a profile centered on the path; the profile placement relative to the path isn't shown, so the gap is unverified |
| 6 | Patterned heatsink | linear pattern, fin spacing | Built: 50 x 40 x 5 base, 8 fins 1.5 x 36 x 25 at 5.5 mm pitch, unioned: 18,640 mm^3 (hand 18,640) |
| 7 | Gear pair with a mate | involute profile, mates | Built by mouse with the new Spur Gear tool: 20T and 30T, module 2, 20 degrees, meshed at the 50 mm center distance; Clearance & Interference reports the pair clear by 0.001 mm |
| 8 | Six-part assembly | joints, interference, explode | Built: plate plus cube, cylinder, sphere, cone and wedge as six instances, a revolute joint, explode to 100%, and Clearance & Interference reporting 1 interfering pair (203.5 mm^3 overlap) |
| 9 | Drawing with GD&T | views, sections, datums, frames | Built on the part 6 heatsink: front view with 50 and 25 mm dimensions, datum A, a position frame to A, Section A-A. Defects below |
| 10 | CAM on a pocketed plate | toolpaths, stock simulation | Built: 80 x 60 x 10 plate, 40 x 30 x 5 cut pocket, 42,000 mm^3 (hand 42,000); rect-pocket toolpath with a 1/8" endmill verified against the part. Moving the pocket 15 mm makes verification fail, as it should |

## Known blockers

- Part 4: a rectangle drawn top-left to bottom-right is a clockwise loop, and
  its extrude is inside out: the viewport shows the interior, the top face
  reports a downward normal, and Shell then produces no geometry. Drawing it
  bottom-left to top-right works. Fixed: extrude normalizes winding.
- Part 6: in a sketch, Cmd-Z undid the last document feature (the base
  extrude), not the last sketch entity. Fixed: undo is scoped to the sketch.
- Part 6: a rectangle whose corners snap to the same x is drawn as a line, and
  the sketch panel showed a raw "add_constraint: horizontal: bad entity handle 0".
  Fixed: refused with a plain message.
- Part 6: with Rectangle armed, a first click on an existing face moves the
  sketch onto that face instead of placing a corner. Fixed: armed tools place
  geometry.
- Part 9: Section A-A is drawn rotated 90 degrees (base on the left, not the
  bottom). Fixed.
- Part 9: the section takes the letter A while datum A exists; section letters
  should skip datum letters. Fixed: one shared alphabet without I, O, Q.
- Part 9: hidden lines included mesh triangle diagonals across flat faces.
  Fixed: front view hidden edges 338 to 58.
- Part 9: new datum and frame symbols land on top of dimension text and
  geometry. Fixed: they slide clear and get a leader.
- The workspace menu in the bottom dock opened downward, below the window. Fixed: it opens upward.
- Part 10: removing a CAM operation crashed the app (Swift exclusivity
  conflict reading the row's binding inside removeAll). Fixed.
- Part 10: Return doesn't commit a CAM number field; only Tab does, so
  Generate can run with the old value. Fixed, checked by mouse.
- Part 10: after editing an operation, the last "Verified against the part"
  stays on screen until Generate runs again. Fixed: marked out of date.
- Drawing workspace with no drawing shows an empty status pill above the dock.
- Part 8: Insert Instance and the clearance report call parts "Part 1" to
  "Part 5" instead of their feature names (Box, Cylinder, ...).
- Part 8: primitives placed by clicking the ground land hundreds of mm from the
  origin, far apart. Fixed: a cube clicked beside a plate landed beside it.
- Part 8: the Assembly dock's interference button stays disabled; the check is
  only reachable from Analyze > Clearance & Interference.
- Part 5: Shell (2 mm, top face open, picked correctly at z = 50, normal +Z)
  on a loft with sloped planar walls evaluates to no geometry. Fixed: loft cap orientation and shell offsets on sloped walls.
- Part 5: after Add Section, the previous loft profile isn't shown, so the
  next one is drawn blind.
- Part 7: with nothing selected, the first gear is placed at the origin, on top
  of existing geometry.
- Part 7: the clearance report gives an overlap of 5,425 mm^3 with a depth of
  0.000 mm, which can't both be right.
- Part 5: Sweep has no field for the helix radius (fixed at 20 mm), labels the
  turns count "mm", and doesn't show where the profile sits on the path. A
  profile wider than the radius sweeps into a self-intersecting solid without
  a warning.
- Part 5: typing into the sweep fields after a triple-click went into the
  previously focused sketch field.
- File > New Document opens a second app instance rather than a new window or
  document in this one.

- Hole panel: Apply with a bolt circle takes several seconds and shows no
  progress. The label column also widens when Counterbore turns on, which moves
  the controls.

- Part 3: `inspect_cad` reports sheet-metal roots as not measurable. An exported
  STL measures 22,734 mm^3 against 22,696 by hand (mid-surface bends).
- Part 3: Flatten refuses a folded four-flange box as a closed section. The
  Author tab's Unfold works, and warns that the flanges meet at the corners.
  There's no corner relief control.

## Fixed

- Hole Apply on a revolved part did nothing: its flat faces reported a zero
  normal. They now report their plane, and a face with no normal is refused.
- Sketches open zoomed in and centered, and the camera returns on exit.
- Hole tool has a bolt circle: one undo step, any hole count.
- Sheet-metal Insert failed on a new document. Flange radius now defaults to
  1.5 mm, which meets the minimum for the default 1.5 mm stock, and each added
  flange takes the next free edge.
- Esc closes a command panel while a number field has focus.
- A boolean on a full 360-degree revolve lost its curved walls. Part 2 now
  measures 18,980.8 mm^3 against 18,981.2 by hand.

## Missing by mouse

Most used first: press-pull, trim and extend, project edges, edit a feature in
place, timeline rollback, construction planes and axes, sketch mirror and
fillet, draft, rib, split, configurations.
