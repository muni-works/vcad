# MCP tool inventory (Phase A)

Every agent tool vcad exposes, where it was implemented before Phase A, whether it was
duplicated, and where it runs after. Design: `docs/launch/mcp-native.md`.

Before Phase A there were three implementations:

- **Swift** — `AgentBridge` in vcad.app (HTTP MCP on 127.0.0.1, `apple/VcadApp/Sources/VcadApp/Agent*.swift`, `*AgentTools.swift`).
- **Rust (M0)** — `crates/vcad-mcp` core tools (`src/tools/core.rs`) plus the node proxy.
- **node** — `packages/mcp` (182 tools, `src/__tests__/tool-surface.fixture.json`).

After Phase A there is one dispatcher, the Rust core, in both vcad.app (linked through
`vcad-ffi`, `AppStore` over the live document) and headless `vcad mcp`. Three kinds of tool:

- **native** — implemented in Rust (`crates/vcad-mcp/src/tools/`), same code in the app and headless.
- **host hook** — needs the viewport, the editor's panels, or the user (camera, pose, snapshot,
  elicitation answered in the viewport, link handshake with `@vcad/mcp`). The core owns the
  listing (`crates/vcad-mcp/src/tools/host_tools.json`) and dispatch; the app's Swift executes
  the call. Headless lists them too and answers with an `isError` result that says vcad.app is
  needed (or, for the five node also implements, hands the call to node), so
  the tool list is identical in both modes.
- **node proxy** — not yet ported; the core spawns `@vcad/mcp` and forwards the call unchanged.

Totals after Phase A: 33 host hook (Swift UI, dispatched by Rust core), 38 native (Rust core), 159 node proxy (230 tools).

| tool | implementations before | Swift file | duplicated | after Phase A | porting cost (TS-only) |
|---|---|---|---|---|---|
| `accept_receipts` | Swift | ProofAgentTools.swift |  | native (Rust core) |  |
| `add_coil` | node |  |  | node proxy | S — ecad.ts ~8 lines, pure TS |
| `add_coil_array` | node |  |  | node proxy | S — ecad.ts ~8 lines, pure TS |
| `add_constraint` | node |  |  | node proxy | S — design-constraints.ts ~18 lines, pure TS |
| `add_motor_winding` | node |  |  | node proxy | S — ecad.ts ~8 lines, pure TS |
| `add_net_tie` | node |  |  | node proxy | S — ecad.ts ~8 lines, pure TS |
| `add_trace` | node |  |  | node proxy | S — ecad.ts ~8 lines, pure TS |
| `add_via` | node |  |  | node proxy | S — ecad.ts ~8 lines, pure TS |
| `add_via_array` | node |  |  | node proxy | S — ecad.ts ~8 lines, pure TS |
| `add_zone` | node |  |  | node proxy | S — ecad.ts ~11 lines, pure TS |
| `adopt_document` | Swift | AgentEdits.swift |  | host hook (Swift UI, dispatched by Rust core) |  |
| `agent_turn` | Swift | AgentTurn.swift |  | host hook (Swift UI, dispatched by Rust core) |  |
| `analyze_antenna` | node |  |  | node proxy | S — antenna.ts ~73 lines, pure TS |
| `analyze_structure` | Swift, node | AnalysisAgentTools.swift | **yes** | host hook (Swift UI, dispatched by Rust core); headless: node proxy |  |
| `analyze_tolerance_stackup` | node |  |  | node proxy | S — tolerance.ts ~53 lines, pure TS |
| `animate` | node |  |  | node proxy | S — animate.ts ~8 lines, pure TS |
| `annotate` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `app_screenshot` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `app_state` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `apply_edits` | node |  |  | node proxy | S — batch-edit.ts ~19 lines, pure TS |
| `ask_user` | Swift | ProofAgentTools.swift |  | host hook (Swift UI, dispatched by Rust core) |  |
| `assemble` | node |  |  | node proxy | S — assemble.ts ~61 lines, pure TS |
| `assert_clearance` | Swift | ProofAgentTools.swift |  | native (Rust core) |  |
| `attach_app_document` | node |  |  | node proxy | S — app-link.ts ~16 lines, pure TS |
| `attach_document` | Swift | AgentEdits.swift |  | host hook (Swift UI, dispatched by Rust core) |  |
| `batch_create_envs` | node |  |  | node proxy | S — gym.ts ~12 lines, pure TS |
| `batch_reset` | node |  |  | node proxy | S — gym.ts ~10 lines, pure TS |
| `batch_step` | node |  |  | node proxy | S — gym.ts ~10 lines, pure TS |
| `beam_check` | node |  |  | node proxy | M — structure.ts ~122 lines, pure TS |
| `board_from_solid` | node |  |  | node proxy | S — ecad.ts ~14 lines, pure TS |
| `bom_add_line` | node |  |  | node proxy | S — bom.ts ~8 lines, pure TS |
| `bom_check_stock` | node |  |  | node proxy | S — part-stock.ts ~9 lines, pure TS |
| `bom_create` | node |  |  | node proxy | S — bom.ts ~8 lines, pure TS |
| `bom_export` | node |  |  | node proxy | S — bom.ts ~9 lines, pure TS |
| `branch_from` | node |  |  | node proxy | S — checkpoint.ts ~13 lines, pure TS |
| `build_receipt` | node |  |  | node proxy | S — ecad.ts ~9 lines, pure TS |
| `calc_coil` | node |  |  | node proxy | S — ecad.ts ~11 lines, pure TS |
| `calc_impedance` | node |  |  | node proxy | S — ecad.ts ~12 lines, pure TS |
| `calc_motor` | node |  |  | node proxy | S — ecad.ts ~16 lines, pure TS |
| `calc_rf` | node |  |  | node proxy | S — ecad.ts ~12 lines, pure TS |
| `call_loon` | node |  |  | node proxy | S — loon-macros.ts ~9 lines, pure TS |
| `check_clearance` | node |  |  | node proxy | S — clearance.ts ~9 lines, pure TS |
| `check_enclosure_fit` | node |  |  | node proxy | S — enclosure.ts ~16 lines, wraps Rust/WASM |
| `check_interference` | Swift | AnalysisAgentTools.swift |  | host hook (Swift UI, dispatched by Rust core) |  |
| `check_joint_range` | node |  |  | node proxy | S — joint-range.ts ~9 lines, pure TS |
| `check_part_stock` | node |  |  | node proxy | S — part-stock.ts ~8 lines, pure TS |
| `check_self_start` | node |  |  | node proxy | S — ecad.ts ~13 lines, pure TS |
| `checkpoint_document` | node |  |  | node proxy | S — checkpoint.ts ~14 lines, pure TS |
| `circular_pattern` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |  |
| `clear_annotations` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `close_document` | Rust (M0), node |  | **yes** | native (Rust core) |  |
| `continue_document` | node |  |  | node proxy | S — continue-doc.ts ~19 lines, pure TS |
| `create` | Swift, node | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) | **yes** | native (Rust core) |  |
| `create_cad_loon` | Swift, Rust (M0), node | AgentEdits.swift | **yes** | native (Rust core) |  |
| `create_robot_env` | node |  |  | node proxy | S — gym.ts ~24 lines, pure TS |
| `create_schematic` | node |  |  | node proxy | S — ecad.ts ~13 lines, pure TS |
| `critique_route` | node |  |  | node proxy | S — ecad.ts ~10 lines, pure TS |
| `define_loon` | node |  |  | node proxy | S — loon-macros.ts ~13 lines, pure TS |
| `delete` | Swift, node | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) | **yes** | native (Rust core) |  |
| `delete_constraint` | node |  |  | node proxy | S — design-constraints.ts ~9 lines, pure TS |
| `delete_net_tie` | node |  |  | node proxy | S — ecad.ts ~8 lines, pure TS |
| `delete_trace` | node |  |  | node proxy | S — ecad.ts ~8 lines, pure TS |
| `delete_via` | node |  |  | node proxy | S — ecad.ts ~8 lines, pure TS |
| `delete_zone` | node |  |  | node proxy | S — ecad.ts ~8 lines, pure TS |
| `describe_pcb` | node |  |  | node proxy | S — ecad.ts ~14 lines, pure TS |
| `describe_scene` | Swift, node | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) | **yes** | native (Rust core) |  |
| `design_bar` | — (new) |  |  | native (Rust core) |  |
| `design_instrument` | — (new) |  |  | native (Rust core) |  |
| `design_material` | node |  |  | node proxy | S — atoms.ts ~8 lines, wraps Rust/WASM |
| `dfm_apply_fix` | Swift, node | AnalysisAgentTools.swift | **yes** | host hook (Swift UI, dispatched by Rust core); headless: node proxy |  |
| `dfm_check` | Swift, node | AnalysisAgentTools.swift | **yes** | host hook (Swift UI, dispatched by Rust core); headless: node proxy |  |
| `dfm_explain` | node |  |  | node proxy | S — dfm.ts ~8 lines, pure TS |
| `dfm_suggest_fix` | node |  |  | node proxy | S — dfm.ts ~8 lines, pure TS |
| `difference` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |  |
| `export_cad` | Rust (M0), node |  | **yes** | native (Rust core) |  |
| `export_gerber` | node |  |  | node proxy | S — ecad.ts ~12 lines, pure TS |
| `export_kicad` | node |  |  | node proxy | S — ecad.ts ~16 lines, pure TS |
| `export_video` | node |  |  | node proxy | S — animate.ts ~9 lines, wraps Rust/WASM |
| `fab_mill` | Rust crate (vcad-atoms, unregistered) |  |  | native (Rust core) |  |
| `fab_prep` | node |  |  | node proxy | S — ecad.ts ~22 lines, pure TS |
| `fab_print` | Rust crate (vcad-atoms, unregistered) |  |  | native (Rust core) |  |
| `fillet_edges` | Swift | AgentResources.swift (AgentViewer) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `find_alternatives` | node |  |  | node proxy | S — ecad.ts ~10 lines, pure TS |
| `fix_drc` | node |  |  | node proxy | S — ecad.ts ~18 lines, pure TS |
| `flat_pattern_from_solid` | node |  |  | node proxy | S — flat-pattern.ts ~9 lines, pure TS |
| `frame_part` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `get_changelog` | node |  |  | node proxy | S — changelog.ts ~10 lines, pure TS |
| `get_copper` | node |  |  | node proxy | S — ecad.ts ~15 lines, pure TS |
| `get_document` | Swift, Rust (M0), node | AgentEdits.swift | **yes** | native (Rust core) |  |
| `get_footprint` | node |  |  | node proxy | S — ecad.ts ~14 lines, pure TS |
| `get_pad_positions` | node |  |  | node proxy | S — ecad.ts ~12 lines, pure TS |
| `get_preview_glb` | node |  |  | node proxy | S — preview.ts ~13 lines, pure TS |
| `get_preview_version` | node |  |  | node proxy | S — preview.ts ~13 lines, pure TS |
| `get_sim_replay` | node |  |  | node proxy | S — sim-replay.ts ~8 lines, pure TS |
| `get_sim_version` | node |  |  | node proxy | S — sim-replay.ts ~9 lines, pure TS |
| `gym_close` | node |  |  | node proxy | S — gym.ts ~7 lines, pure TS |
| `gym_observe` | node |  |  | node proxy | S — gym.ts ~10 lines, pure TS |
| `gym_reset` | node |  |  | node proxy | S — gym.ts ~12 lines, pure TS |
| `gym_step` | node |  |  | node proxy | S — gym.ts ~19 lines, pure TS |
| `highlight_parts` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `history_seek` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `homogenize_material` | node |  |  | node proxy | S — atoms.ts ~8 lines, pure TS |
| `import_altium` | node |  |  | node proxy | S — import-pcb.ts ~14 lines, pure TS |
| `import_altium_library` | node |  |  | node proxy | S — import-pcb.ts ~10 lines, pure TS |
| `import_board` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `import_eagle` | node |  |  | node proxy | S — import-pcb.ts ~13 lines, pure TS |
| `import_kicad` | node |  |  | node proxy | S — import-pcb.ts ~12 lines, pure TS |
| `import_step` | node |  |  | node proxy | S — import.ts ~13 lines, pure TS |
| `import_urdf` | node |  |  | node proxy | S — import-urdf.ts ~16 lines, pure TS |
| `inspect` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |  |
| `inspect_cad` | Swift, Rust (M0), node | AgentEdits.swift | **yes** | native (Rust core) |  |
| `inspect_faces` | Swift, node | AnalysisAgentTools.swift | **yes** | host hook (Swift UI, dispatched by Rust core); headless: node proxy |  |
| `inspect_molecule` | node |  |  | node proxy | S — atoms.ts ~8 lines, pure TS |
| `inspect_part` | node |  |  | node proxy | ? (not found) |
| `inspect_schematic` | node |  |  | node proxy | S — ecad.ts ~10 lines, pure TS |
| `instrument_receipt` | — (new) |  |  | native (Rust core) |  |
| `intersection` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |  |
| `length_match_traces` | node |  |  | node proxy | S — ecad.ts ~14 lines, pure TS |
| `linear_pattern` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |  |
| `link_status` | Swift | AgentEdits.swift |  | host hook (Swift UI, dispatched by Rust core) |  |
| `list_constraints` | node |  |  | node proxy | S — design-constraints.ts ~11 lines, pure TS |
| `list_eval_tasks` | node |  |  | node proxy | S — verify.ts ~9 lines, wraps Rust/WASM |
| `list_footprints` | node |  |  | node proxy | S — ecad.ts ~11 lines, pure TS |
| `list_loons` | node |  |  | node proxy | S — loon-macros.ts ~10 lines, pure TS |
| `list_parameters` | Swift, node | AgentEdits.swift | **yes** | native (Rust core) |  |
| `list_parts` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `list_tool_packs` | node |  |  | node proxy | S — server.ts ~20 lines, pure TS |
| `load_document` | Rust (M0), node |  | **yes** | native (Rust core) |  |
| `load_structure` | node |  |  | node proxy | S — atoms.ts ~8 lines, pure TS |
| `md_run` | node |  |  | node proxy | S — atoms.ts ~8 lines, pure TS |
| `measure` | node |  |  | node proxy | S — measure.ts ~9 lines, wraps Rust/WASM |
| `measure_outer_diameter` | node |  |  | node proxy | S — faces.ts ~9 lines, pure TS |
| `minimize_energy` | node |  |  | node proxy | S — atoms.ts ~8 lines, pure TS |
| `mirror` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |  |
| `open_document` | Swift, Rust (M0), node | AgentBridge.swift (AgentTools) | **yes** | native (Rust core) |  |
| `open_in_app` | Swift, node | AgentResources.swift (AgentViewer) | **yes** | host hook (Swift UI, dispatched by Rust core); headless: node proxy |  |
| `optimize_electrodes` | node |  |  | node proxy | S — particle.ts ~27 lines, pure TS |
| `optimize_parameters` | Rust crate (vcad-atoms, unregistered) |  |  | native (Rust core) |  |
| `parameter_gradient` | node |  |  | node proxy | S — parameters.ts ~8 lines, pure TS |
| `photoreal` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `pick_in_viewport` | Swift | ProofAgentTools.swift |  | host hook (Swift UI, dispatched by Rust core) |  |
| `place` | Swift, node | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) | **yes** | native (Rust core) |  |
| `place_components` | node |  |  | node proxy | S — ecad.ts ~23 lines, pure TS |
| `place_part` | node |  |  | node proxy | S — parts.ts ~9 lines, pure TS |
| `polyline_tube` | node |  |  | node proxy | ? (not found) |
| `predict_physics` | node |  |  | node proxy | S — physics.ts ~16 lines, pure TS |
| `predict_print` | node |  |  | node proxy | S — print-check.ts ~8 lines, wraps Rust/WASM |
| `press_key` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `quote_manufacturing` | node |  |  | node proxy | S — order.ts ~9 lines, pure TS |
| `read` | node |  |  | node proxy | ? (not found) |
| `record_measurement` | node |  |  | node proxy | S — print-check.ts ~9 lines, pure TS |
| `record_simulation` | node |  |  | node proxy | S — record.ts ~14 lines, wraps Rust/WASM |
| `record_strike` | — (new) |  |  | native (Rust core) |  |
| `render_molecule` | node |  |  | node proxy | S — atoms.ts ~9 lines, pure TS |
| `render_pcb` | node |  |  | node proxy | S — render.ts ~11 lines, pure TS |
| `render_ratsnest` | node |  |  | node proxy | S — render.ts ~11 lines, pure TS |
| `render_sequence` | node |  |  | node proxy | S — animate.ts ~10 lines, wraps Rust/WASM |
| `render_stackup` | node |  |  | node proxy | S — render.ts ~12 lines, pure TS |
| `render_view` | Rust (M0), node |  | **yes** | native (Rust core) |  |
| `resolve_part` | node |  |  | node proxy | S — ecad.ts ~10 lines, pure TS |
| `rotate` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |  |
| `route_diff_pair` | node |  |  | node proxy | S — ecad.ts ~10 lines, pure TS |
| `route_nets` | node |  |  | node proxy | S — ecad.ts ~20 lines, pure TS |
| `run_drc` | node |  |  | node proxy | S — ecad.ts ~8 lines, pure TS |
| `run_erc` | node |  |  | node proxy | S — ecad.ts ~9 lines, pure TS |
| `save_document` | node |  |  | node proxy | S — session.ts ~12 lines, pure TS |
| `scale` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |  |
| `search_electronic_parts` | node |  |  | node proxy | S — ecad.ts ~12 lines, pure TS |
| `search_footprints` | node |  |  | node proxy | S — ecad.ts ~10 lines, pure TS |
| `search_mechanical_parts` | node |  |  | node proxy | S — mech-parts.ts ~9 lines, pure TS |
| `search_parts` | node |  |  | node proxy | S — parts.ts ~8 lines, pure TS |
| `select_edges` | Swift | AgentResources.swift (AgentViewer) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `select_feature` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `sensitivity` | node |  |  | node proxy | S — parameters.ts ~9 lines, pure TS |
| `server_info` | Rust (M0), node |  | **yes** | native (Rust core) |  |
| `set_board_outline` | node |  |  | node proxy | S — ecad.ts ~8 lines, pure TS |
| `set_camera` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `set_design_rules` | node |  |  | node proxy | S — ecad.ts ~11 lines, pure TS |
| `set_explode` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `set_material` | Swift, node | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) | **yes** | native (Rust core) |  |
| `set_parameters` | Swift, node | AgentEdits.swift | **yes** | native (Rust core) |  |
| `set_placement` | node |  |  | node proxy | S — ecad.ts ~14 lines, pure TS |
| `set_pose` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `set_stackup` | node |  |  | node proxy | S — ecad.ts ~8 lines, pure TS |
| `set_tool_packs` | node |  |  | node proxy | M — server.ts ~184 lines, wraps Rust/WASM |
| `set_workspace` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `sheet_metal_bend_table` | node |  |  | node proxy | S — sheet-metal.ts ~8 lines, wraps Rust/WASM |
| `sheet_metal_check` | node |  |  | node proxy | S — sheet-metal.ts ~8 lines, pure TS |
| `sheet_metal_cost` | node |  |  | node proxy | S — sheet-metal.ts ~8 lines, pure TS |
| `sheet_metal_create` | node |  |  | node proxy | S — sheet-metal.ts ~8 lines, pure TS |
| `sheet_metal_materials` | node |  |  | node proxy | S — sheet-metal.ts ~8 lines, pure TS |
| `sheet_metal_nest` | node |  |  | node proxy | S — sheet-metal.ts ~9 lines, pure TS |
| `sheet_metal_sequence` | node |  |  | node proxy | S — sheet-metal.ts ~8 lines, pure TS |
| `sheet_metal_suggest_fix` | node |  |  | node proxy | S — sheet-metal.ts ~8 lines, pure TS |
| `sheet_metal_unfold` | node |  |  | node proxy | S — sheet-metal.ts ~8 lines, pure TS |
| `show_in_app` | node |  |  | node proxy | M — app-link.ts ~131 lines, pure TS |
| `simulate_charged_particles` | node |  |  | node proxy | S — particle.ts ~25 lines, pure TS |
| `simulate_circuit` | node |  |  | node proxy | M — circuit.ts ~130 lines, pure TS |
| `simulate_em` | node |  |  | node proxy | M — em.ts ~141 lines, pure TS |
| `simulate_flow` | node |  |  | node proxy | S — flow.ts ~69 lines, pure TS |
| `simulate_lattice_gauge` | node |  |  | node proxy | S — qcd.ts ~31 lines, pure TS |
| `simulate_neutron_shield` | node |  |  | node proxy | S — neutronics.ts ~37 lines, pure TS |
| `simulate_photonics` | node |  |  | node proxy | S — photonics.ts ~62 lines, pure TS |
| `simulate_strike` | node |  |  | node proxy | S — acoustics.ts ~9 lines, wraps Rust/WASM |
| `size_coil` | node |  |  | node proxy | S — ecad.ts ~11 lines, pure TS |
| `size_impedance` | node |  |  | node proxy | S — ecad.ts ~13 lines, pure TS |
| `size_pdn` | node |  |  | node proxy | S — ecad.ts ~15 lines, pure TS |
| `size_trace_for_current` | node |  |  | node proxy | S — ecad.ts ~12 lines, pure TS |
| `snapshot` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `solid_from_board` | node |  |  | node proxy | S — ecad.ts ~52 lines, wraps Rust/WASM |
| `solve_constraints` | node |  |  | node proxy | S — design-constraints.ts ~12 lines, pure TS |
| `solve_thermal` | node |  |  | node proxy | S — thermal.ts ~62 lines, pure TS |
| `suggest_edit` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `timeline_from_simulation` | node |  |  | node proxy | S — animate.ts ~8 lines, pure TS |
| `topology_optimize` | node |  |  | node proxy | S — topopt.ts ~17 lines, pure TS |
| `translate` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |  |
| `tube` | node |  |  | node proxy | ? (not found) |
| `tune_circuit` | node |  |  | node proxy | S — circuit.ts ~79 lines, pure TS |
| `undo` | Swift, node | AgentEdits.swift | **yes** | native (Rust core) |  |
| `union` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |  |
| `update` | Swift, node | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) | **yes** | native (Rust core) |  |
| `update_schematic` | node |  |  | node proxy | S — ecad.ts ~11 lines, pure TS |
| `validate_for_fab` | node |  |  | node proxy | S — ecad.ts ~16 lines, pure TS |
| `verify_part` | node |  |  | node proxy | S — verify.ts ~8 lines, pure TS |
| `verify_receipt` | node |  |  | node proxy | S — ecad.ts ~8 lines, pure TS |
| `verify_receipts` | Swift | ProofAgentTools.swift |  | native (Rust core) |  |
| `verify_spec` | node |  |  | node proxy | S — verify-spec.ts ~9 lines, wraps Rust/WASM |
| `verify_substitution` | node |  |  | node proxy | S — ecad.ts ~13 lines, pure TS |
| `viewer_frame` | Swift | AgentResources.swift (AgentViewer) |  | host hook (Swift UI, dispatched by Rust core) |  |
| `winding_layout` | node |  |  | node proxy | S — ecad.ts ~12 lines, pure TS |

## Porting cost (TS-only tools)

Node is being retired as an implementation: `@vcad/mcp` stays as a launcher for the prebuilt Rust `vcad mcp`, and TS-only tools are proxied until ported or dropped. The last column is a first-cut estimate per tool: **S** < 120 lines of TS for the tool's definition+handler, **M** < 500, **L** larger (measured from the tool's span in its source file, so shared helpers are not counted); "wraps Rust/WASM" means the handler mostly calls kernel/WASM code that already exists in Rust, so the port is mostly plumbing. Totals: M 5, S 150. Decide port vs drop per row; tools tied to retired hosted services are already gone.

## Notes on the duplicates

- `open_document`: Swift opened a file path in the window; Rust/node minted a session from `initial`. The native tool takes either (`path` → the app opens it in the window; headless loads the file).
- `get_document`, `inspect_cad`, `list_parameters`, `set_parameters`, `undo`: node/Rust required `document_id`; Swift used the open window. Native tools take `document_id` and fall back to the store's current document (the front window in the app). `set_parameters` accepts node's `parameters` and Swift's `values`.
- Engine edit tools (`create`, `update`, `delete`, `set_material`, booleans, transforms, patterns): Swift and node both had them. Native now: `vcad-app`'s lowering applied to the stored document, committed as one named undo step. Arguments the engine lowering doesn't cover (node-only types, `describe_scene` with `joint_state`) delegate to node unchanged.
- Analysis (`dfm_check`, `dfm_apply_fix`, `inspect_faces`, `analyze_structure`): node and Swift both. The app keeps them as host hooks because they drive the Analysis panel and overlays (their compute is already Rust in `vcad-ffi`); headless they proxy to node. Folding the panel-free compute into a shared crate is a follow-up.
- Receipts: `verify_receipts`, `accept_receipts`, `assert_clearance` are native over `vcad-receipt-verify`; the viewport badges follow the document revision as before. node's `build_receipt` / `verify_receipt` stay proxied (different, broader claim families).
- `fab_print`, `fab_mill`, `optimize_parameters` (`vcad-atoms`): native long-running tools — MCP Tasks with progress and cancel.
