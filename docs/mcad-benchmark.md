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
| 5 | Swept and lofted duct | sweep, loft, shell | Open |
| 6 | Patterned heatsink | linear pattern, fin spacing | Built: 50 x 40 x 5 base, 8 fins 1.5 x 36 x 25 at 5.5 mm pitch, unioned: 18,640 mm^3 (hand 18,640) |
| 7 | Gear pair with a mate | involute profile, mates | Open |
| 8 | Six-part assembly | joints, interference, explode | Open |
| 9 | Drawing with GD&T | views, sections, datums, frames | Open |
| 10 | CAM on a pocketed plate | toolpaths, stock simulation | Open |

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
