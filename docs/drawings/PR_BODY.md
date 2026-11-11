## One-click drawings

**What was there:** vcad-kernel-drafting (projection, hidden-line removal, sections, details, GD&T, title block, PDF/DXF) plus a single-view sheet. Details and GD&T weren't saved, dimensions were only overall size, and DXF covered one view.

**Built:**
- **Standard sheet (default):** front/top/right/iso in third-angle layout. The sheet size (A4→A1) and a standard scale are picked automatically, and the iso view gets its own scale. The title block fills in from the document.
- **Automatic dimensions:** overall size, plus circles recognized from projected edge loops. Those drive center marks, `N× ⌀D` callouts and hole locations on single parts.
- **Editing:** Dimension tool (snaps to corners and hole centers), Center Line tool, drag to move, ⌫ to delete or hide, restore, sheet-size picker, toggles.
- **Saved with the document:** IR `DrawingSettings` gains dimensions, hidden and moved auto dimensions, details, GD&T, sheet size and toggles. They're stored through the drawing-settings feature's `annotations` field, so they undo and save.
- **Export:** vector PDF, and a sheet DXF in model mm with views at true size (tested).
- **Associative:** the sheet is rebuilt from the model on every change (tested).

**Verification (local; CI billing is broken):**
- Rust on mew: drafting/ir/app/ffi tests pass; clippy -D warnings and fmt are clean; ir:gen regenerated.
- Swift: `swift test` ran 366 tests with 0 failures.
- Samples in `docs/drawings/`: cube, motor-flange, and the k1-pto assembly (PDF + PNG for each).

**Gaps:**
- User dimensions are anchored to view coordinates, not topology.
- No automatic center lines in side views.
- Callouts give diameter only, no depth.
- No in-app screenshot: screen capture was blocked here.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
