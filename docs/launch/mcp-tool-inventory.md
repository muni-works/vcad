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

Totals after Phase A: 33 host hook (Swift UI, dispatched by Rust core), 34 native (Rust core), 159 node proxy (226 tools).

| tool | implementations before | Swift file | duplicated | after Phase A |
|---|---|---|---|---|
| `accept_receipts` | Swift | ProofAgentTools.swift |  | native (Rust core) |
| `add_coil` | node |  |  | node proxy |
| `add_coil_array` | node |  |  | node proxy |
| `add_constraint` | node |  |  | node proxy |
| `add_motor_winding` | node |  |  | node proxy |
| `add_net_tie` | node |  |  | node proxy |
| `add_trace` | node |  |  | node proxy |
| `add_via` | node |  |  | node proxy |
| `add_via_array` | node |  |  | node proxy |
| `add_zone` | node |  |  | node proxy |
| `adopt_document` | Swift | AgentEdits.swift |  | host hook (Swift UI, dispatched by Rust core) |
| `agent_turn` | Swift | AgentTurn.swift |  | host hook (Swift UI, dispatched by Rust core) |
| `analyze_antenna` | node |  |  | node proxy |
| `analyze_structure` | Swift, node | AnalysisAgentTools.swift | **yes** | host hook (Swift UI, dispatched by Rust core); headless: node proxy |
| `analyze_tolerance_stackup` | node |  |  | node proxy |
| `animate` | node |  |  | node proxy |
| `annotate` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `app_screenshot` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `app_state` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `apply_edits` | node |  |  | node proxy |
| `ask_user` | Swift | ProofAgentTools.swift |  | host hook (Swift UI, dispatched by Rust core) |
| `assemble` | node |  |  | node proxy |
| `assert_clearance` | Swift | ProofAgentTools.swift |  | native (Rust core) |
| `attach_app_document` | node |  |  | node proxy |
| `attach_document` | Swift | AgentEdits.swift |  | host hook (Swift UI, dispatched by Rust core) |
| `batch_create_envs` | node |  |  | node proxy |
| `batch_reset` | node |  |  | node proxy |
| `batch_step` | node |  |  | node proxy |
| `beam_check` | node |  |  | node proxy |
| `board_from_solid` | node |  |  | node proxy |
| `bom_add_line` | node |  |  | node proxy |
| `bom_check_stock` | node |  |  | node proxy |
| `bom_create` | node |  |  | node proxy |
| `bom_export` | node |  |  | node proxy |
| `branch_from` | node |  |  | node proxy |
| `build_receipt` | node |  |  | node proxy |
| `calc_coil` | node |  |  | node proxy |
| `calc_impedance` | node |  |  | node proxy |
| `calc_motor` | node |  |  | node proxy |
| `calc_rf` | node |  |  | node proxy |
| `call_loon` | node |  |  | node proxy |
| `check_clearance` | node |  |  | node proxy |
| `check_enclosure_fit` | node |  |  | node proxy |
| `check_interference` | Swift | AnalysisAgentTools.swift |  | host hook (Swift UI, dispatched by Rust core) |
| `check_joint_range` | node |  |  | node proxy |
| `check_part_stock` | node |  |  | node proxy |
| `check_self_start` | node |  |  | node proxy |
| `checkpoint_document` | node |  |  | node proxy |
| `circular_pattern` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |
| `clear_annotations` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `close_document` | Rust (M0), node |  | **yes** | native (Rust core) |
| `continue_document` | node |  |  | node proxy |
| `create` | Swift, node | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) | **yes** | native (Rust core) |
| `create_cad_loon` | Swift, Rust (M0), node | AgentEdits.swift | **yes** | native (Rust core) |
| `create_robot_env` | node |  |  | node proxy |
| `create_schematic` | node |  |  | node proxy |
| `critique_route` | node |  |  | node proxy |
| `define_loon` | node |  |  | node proxy |
| `delete` | Swift, node | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) | **yes** | native (Rust core) |
| `delete_constraint` | node |  |  | node proxy |
| `delete_net_tie` | node |  |  | node proxy |
| `delete_trace` | node |  |  | node proxy |
| `delete_via` | node |  |  | node proxy |
| `delete_zone` | node |  |  | node proxy |
| `describe_pcb` | node |  |  | node proxy |
| `describe_scene` | Swift, node | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) | **yes** | native (Rust core) |
| `design_material` | node |  |  | node proxy |
| `dfm_apply_fix` | Swift, node | AnalysisAgentTools.swift | **yes** | host hook (Swift UI, dispatched by Rust core); headless: node proxy |
| `dfm_check` | Swift, node | AnalysisAgentTools.swift | **yes** | host hook (Swift UI, dispatched by Rust core); headless: node proxy |
| `dfm_explain` | node |  |  | node proxy |
| `dfm_suggest_fix` | node |  |  | node proxy |
| `difference` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |
| `export_cad` | Rust (M0), node |  | **yes** | native (Rust core) |
| `export_gerber` | node |  |  | node proxy |
| `export_kicad` | node |  |  | node proxy |
| `export_video` | node |  |  | node proxy |
| `fab_mill` | Rust crate (vcad-atoms, unregistered) |  |  | native (Rust core) |
| `fab_prep` | node |  |  | node proxy |
| `fab_print` | Rust crate (vcad-atoms, unregistered) |  |  | native (Rust core) |
| `fillet_edges` | Swift | AgentResources.swift (AgentViewer) |  | host hook (Swift UI, dispatched by Rust core) |
| `find_alternatives` | node |  |  | node proxy |
| `fix_drc` | node |  |  | node proxy |
| `flat_pattern_from_solid` | node |  |  | node proxy |
| `frame_part` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `get_changelog` | node |  |  | node proxy |
| `get_copper` | node |  |  | node proxy |
| `get_document` | Swift, Rust (M0), node | AgentEdits.swift | **yes** | native (Rust core) |
| `get_footprint` | node |  |  | node proxy |
| `get_pad_positions` | node |  |  | node proxy |
| `get_preview_glb` | node |  |  | node proxy |
| `get_preview_version` | node |  |  | node proxy |
| `get_sim_replay` | node |  |  | node proxy |
| `get_sim_version` | node |  |  | node proxy |
| `gym_close` | node |  |  | node proxy |
| `gym_observe` | node |  |  | node proxy |
| `gym_reset` | node |  |  | node proxy |
| `gym_step` | node |  |  | node proxy |
| `highlight_parts` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `history_seek` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `homogenize_material` | node |  |  | node proxy |
| `import_altium` | node |  |  | node proxy |
| `import_altium_library` | node |  |  | node proxy |
| `import_board` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `import_eagle` | node |  |  | node proxy |
| `import_kicad` | node |  |  | node proxy |
| `import_step` | node |  |  | node proxy |
| `import_urdf` | node |  |  | node proxy |
| `inspect` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |
| `inspect_cad` | Swift, Rust (M0), node | AgentEdits.swift | **yes** | native (Rust core) |
| `inspect_faces` | Swift, node | AnalysisAgentTools.swift | **yes** | host hook (Swift UI, dispatched by Rust core); headless: node proxy |
| `inspect_molecule` | node |  |  | node proxy |
| `inspect_part` | node |  |  | node proxy |
| `inspect_schematic` | node |  |  | node proxy |
| `intersection` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |
| `length_match_traces` | node |  |  | node proxy |
| `linear_pattern` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |
| `link_status` | Swift | AgentEdits.swift |  | host hook (Swift UI, dispatched by Rust core) |
| `list_constraints` | node |  |  | node proxy |
| `list_eval_tasks` | node |  |  | node proxy |
| `list_footprints` | node |  |  | node proxy |
| `list_loons` | node |  |  | node proxy |
| `list_parameters` | Swift, node | AgentEdits.swift | **yes** | native (Rust core) |
| `list_parts` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `list_tool_packs` | node |  |  | node proxy |
| `load_document` | Rust (M0), node |  | **yes** | native (Rust core) |
| `load_structure` | node |  |  | node proxy |
| `md_run` | node |  |  | node proxy |
| `measure` | node |  |  | node proxy |
| `measure_outer_diameter` | node |  |  | node proxy |
| `minimize_energy` | node |  |  | node proxy |
| `mirror` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |
| `open_document` | Swift, Rust (M0), node | AgentBridge.swift (AgentTools) | **yes** | native (Rust core) |
| `open_in_app` | Swift, node | AgentResources.swift (AgentViewer) | **yes** | host hook (Swift UI, dispatched by Rust core); headless: node proxy |
| `optimize_electrodes` | node |  |  | node proxy |
| `optimize_parameters` | Rust crate (vcad-atoms, unregistered) |  |  | native (Rust core) |
| `parameter_gradient` | node |  |  | node proxy |
| `photoreal` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `pick_in_viewport` | Swift | ProofAgentTools.swift |  | host hook (Swift UI, dispatched by Rust core) |
| `place` | Swift, node | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) | **yes** | native (Rust core) |
| `place_components` | node |  |  | node proxy |
| `place_part` | node |  |  | node proxy |
| `polyline_tube` | node |  |  | node proxy |
| `predict_physics` | node |  |  | node proxy |
| `predict_print` | node |  |  | node proxy |
| `press_key` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `quote_manufacturing` | node |  |  | node proxy |
| `read` | node |  |  | node proxy |
| `record_measurement` | node |  |  | node proxy |
| `record_simulation` | node |  |  | node proxy |
| `render_molecule` | node |  |  | node proxy |
| `render_pcb` | node |  |  | node proxy |
| `render_ratsnest` | node |  |  | node proxy |
| `render_sequence` | node |  |  | node proxy |
| `render_stackup` | node |  |  | node proxy |
| `render_view` | Rust (M0), node |  | **yes** | native (Rust core) |
| `resolve_part` | node |  |  | node proxy |
| `rotate` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |
| `route_diff_pair` | node |  |  | node proxy |
| `route_nets` | node |  |  | node proxy |
| `run_drc` | node |  |  | node proxy |
| `run_erc` | node |  |  | node proxy |
| `save_document` | node |  |  | node proxy |
| `scale` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |
| `search_electronic_parts` | node |  |  | node proxy |
| `search_footprints` | node |  |  | node proxy |
| `search_mechanical_parts` | node |  |  | node proxy |
| `search_parts` | node |  |  | node proxy |
| `select_edges` | Swift | AgentResources.swift (AgentViewer) |  | host hook (Swift UI, dispatched by Rust core) |
| `select_feature` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `sensitivity` | node |  |  | node proxy |
| `server_info` | Rust (M0), node |  | **yes** | native (Rust core) |
| `set_board_outline` | node |  |  | node proxy |
| `set_camera` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `set_design_rules` | node |  |  | node proxy |
| `set_explode` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `set_material` | Swift, node | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) | **yes** | native (Rust core) |
| `set_parameters` | Swift, node | AgentEdits.swift | **yes** | native (Rust core) |
| `set_placement` | node |  |  | node proxy |
| `set_pose` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `set_stackup` | node |  |  | node proxy |
| `set_tool_packs` | node |  |  | node proxy |
| `set_workspace` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `sheet_metal_bend_table` | node |  |  | node proxy |
| `sheet_metal_check` | node |  |  | node proxy |
| `sheet_metal_cost` | node |  |  | node proxy |
| `sheet_metal_create` | node |  |  | node proxy |
| `sheet_metal_materials` | node |  |  | node proxy |
| `sheet_metal_nest` | node |  |  | node proxy |
| `sheet_metal_sequence` | node |  |  | node proxy |
| `sheet_metal_suggest_fix` | node |  |  | node proxy |
| `sheet_metal_unfold` | node |  |  | node proxy |
| `show_in_app` | node |  |  | node proxy |
| `simulate_charged_particles` | node |  |  | node proxy |
| `simulate_circuit` | node |  |  | node proxy |
| `simulate_em` | node |  |  | node proxy |
| `simulate_flow` | node |  |  | node proxy |
| `simulate_lattice_gauge` | node |  |  | node proxy |
| `simulate_neutron_shield` | node |  |  | node proxy |
| `simulate_photonics` | node |  |  | node proxy |
| `simulate_strike` | node |  |  | node proxy |
| `size_coil` | node |  |  | node proxy |
| `size_impedance` | node |  |  | node proxy |
| `size_pdn` | node |  |  | node proxy |
| `size_trace_for_current` | node |  |  | node proxy |
| `snapshot` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `solid_from_board` | node |  |  | node proxy |
| `solve_constraints` | node |  |  | node proxy |
| `solve_thermal` | node |  |  | node proxy |
| `suggest_edit` | Swift | AgentBridge.swift (AgentTools) |  | host hook (Swift UI, dispatched by Rust core) |
| `timeline_from_simulation` | node |  |  | node proxy |
| `topology_optimize` | node |  |  | node proxy |
| `translate` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |
| `tube` | node |  |  | node proxy |
| `tune_circuit` | node |  |  | node proxy |
| `undo` | Swift, node | AgentEdits.swift | **yes** | native (Rust core) |
| `union` | Swift | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) |  | native (Rust core) |
| `update` | Swift, node | AgentEdits.swift → vcad-ffi `vcad_engine_apply_tool` (vcad-app lowering) | **yes** | native (Rust core) |
| `update_schematic` | node |  |  | node proxy |
| `validate_for_fab` | node |  |  | node proxy |
| `verify_part` | node |  |  | node proxy |
| `verify_receipt` | node |  |  | node proxy |
| `verify_receipts` | Swift | ProofAgentTools.swift |  | native (Rust core) |
| `verify_spec` | node |  |  | node proxy |
| `verify_substitution` | node |  |  | node proxy |
| `viewer_frame` | Swift | AgentResources.swift (AgentViewer) |  | host hook (Swift UI, dispatched by Rust core) |
| `winding_layout` | node |  |  | node proxy |

## Notes on the duplicates

- `open_document`: Swift opened a file path in the window; Rust/node minted a session from `initial`. The native tool takes either (`path` → the app opens it in the window; headless loads the file).
- `get_document`, `inspect_cad`, `list_parameters`, `set_parameters`, `undo`: node/Rust required `document_id`; Swift used the open window. Native tools take `document_id` and fall back to the store's current document (the front window in the app). `set_parameters` accepts node's `parameters` and Swift's `values`.
- Engine edit tools (`create`, `update`, `delete`, `set_material`, booleans, transforms, patterns): Swift and node both had them. Native now: `vcad-app`'s lowering applied to the stored document, committed as one named undo step. Arguments the engine lowering doesn't cover (node-only types, `describe_scene` with `joint_state`) delegate to node unchanged.
- Analysis (`dfm_check`, `dfm_apply_fix`, `inspect_faces`, `analyze_structure`): node and Swift both. The app keeps them as host hooks because they drive the Analysis panel and overlays (their compute is already Rust in `vcad-ffi`); headless they proxy to node. Folding the panel-free compute into a shared crate is a follow-up.
- Receipts: `verify_receipts`, `accept_receipts`, `assert_clearance` are native over `vcad-receipt-verify`; the viewport badges follow the document revision as before. node's `build_receipt` / `verify_receipt` stay proxied (different, broader claim families).
- `fab_print`, `fab_mill`, `optimize_parameters` (`vcad-atoms`): native long-running tools — MCP Tasks with progress and cancel.
