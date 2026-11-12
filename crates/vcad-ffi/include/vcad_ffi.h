/* vcad-ffi — C ABI for the vcad kernel. Canonical header; kept in sync with
 * crates/vcad-ffi/src/lib.rs by hand for now (cbindgen automation is a TODO).
 * Buffer layout matches vcad_kernel_tessellate::TriangleMesh: flat f32 triples
 * for positions/normals, flat u32 indices. Lengths are element counts. */
#ifndef VCAD_FFI_H
#define VCAD_FFI_H

#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Rectangular CAM: JSON request -> owned UTF-8 JSON (gcode, moves).
 * Null on failure; read vcad_last_error. Free only with vcad_cam_free. */
char *vcad_cam_generate(const char *request_json);
void vcad_cam_free(char *json);

/* Document engine: the shared DocCommand vocabulary (see vcad_app::command).
 * vcad_engine_apply takes a UTF-8 JSON command and returns an owned JSON
 * envelope {"ok":true,"result":{...}} or {"ok":false,"error":{...}};
 * null only for a null argument or panic (read vcad_last_error).
 * Free results with vcad_engine_string_free, engines with vcad_engine_free. */
typedef struct VcadEngine VcadEngine;
VcadEngine *vcad_engine_new(uint64_t replica_id);
char *vcad_engine_apply(VcadEngine *engine, const char *command_json);
/* Lower an AI tool call (create, translate, linear_pattern, ...) in Rust and
 * apply it; same envelope. Unhandled tools report error kind "unsupported". */
char *vcad_engine_apply_tool(VcadEngine *engine, const char *tool, const char *args_json);
/* Same as vcad_engine_apply / vcad_engine_apply_tool, answered with an
 * EngineDeltaEnvelope (what changed since the state before the command:
 * a document patch + parts added/updated/removed by stable id). */
char *vcad_engine_apply_delta(VcadEngine *engine, const char *command_json);
char *vcad_engine_apply_tool_delta(VcadEngine *engine, const char *tool, const char *args_json);
/* Deterministic state fingerprint (16 hex digits); free with vcad_engine_string_free. */
char *vcad_engine_state_hash(const VcadEngine *engine);
/* Current state as an ok envelope; free with vcad_engine_string_free. */
char *vcad_engine_state(const VcadEngine *engine);
/* Replace the document with an IR Document JSON (a .vcad file), fresh undo
 * history. Returns the state envelope; null (engine untouched) when the JSON
 * is not an IR document. */
char *vcad_engine_load_ir(VcadEngine *engine, const char *ir_json);
/* Snapshot document + undo/redo history; free with vcad_engine_bytes_free. */
uint8_t *vcad_engine_save(const VcadEngine *engine, size_t *out_len);
void vcad_engine_bytes_free(uint8_t *bytes, size_t len);
/* Restore a vcad_engine_save snapshot; state envelope, or null on bad bytes. */
char *vcad_engine_restore(VcadEngine *engine, const uint8_t *bytes, size_t len);
bool vcad_engine_can_undo(const VcadEngine *engine);
bool vcad_engine_can_redo(const VcadEngine *engine);
void vcad_engine_string_free(char *json);
void vcad_engine_free(VcadEngine *engine);

/* Document runtime (docs/runtime-design.md). A VcadEngine is a runtime: its
 * vcad_engine_* calls and the intents queued here run in order on one
 * thread. Frames arrive through the sink ON THE RUNTIME THREAD; the host owns
 * each frame and frees it with vcad_frame_free. vcad_engine_free clears the
 * sink and joins the thread: no callback runs after it returns. */
typedef struct VcadFrame VcadFrame;
typedef struct VcadScene VcadScene;
typedef void (*VcadFrameFn)(void *ctx, VcadFrame *frame);
void vcad_doc_runtime_set_sink(const VcadEngine *engine, VcadFrameFn cb, void *ctx);
/* {"seq":N,"intent":{"type":"set_doc_param"|"command"|"gesture_end"|"publish",...}}; never blocks.
 * Blocking edits also publish a frame of their revision once the inbox is idle. */
bool vcad_doc_runtime_send(const VcadEngine *engine, const uint8_t *json, size_t len);
uint64_t vcad_doc_runtime_rev(const VcadEngine *engine);
/* Undo / redo a whole gesture; state envelope (vcad_engine_string_free) or null. */
char *vcad_doc_runtime_swap_gesture(const VcadEngine *engine, uint64_t gesture);
/* Current state's scene through the runtime's incremental cache; blocks. vcad_scene_free. */
VcadScene *vcad_doc_runtime_scene(const VcadEngine *engine);
/* vcad_sound_call on the runtime's document + evaluated meshes; solves on the
 * calling thread (call off the UI thread). vcad_string_free. */
char *vcad_doc_runtime_sound(const VcadEngine *engine, const char *request_json);
void vcad_doc_runtime_forget_gesture(const VcadEngine *engine, uint64_t gesture);
void vcad_doc_runtime_set_base_dir(const VcadEngine *engine, const uint8_t *dir, size_t len);
uint64_t vcad_frame_rev(const VcadFrame *frame);
uint64_t vcad_frame_acked_seq(const VcadFrame *frame);
const char *vcad_frame_state(const VcadFrame *frame);
const char *vcad_frame_info(const VcadFrame *frame);
const uint8_t *vcad_frame_document_json(const VcadFrame *frame, size_t *out_len);
VcadScene *vcad_frame_take_scene(VcadFrame *frame);
void vcad_frame_free(VcadFrame *frame);
/* Built-in agent: tool manifest + system prompt JSON (free with
 * vcad_engine_string_free), and read tools (describe_scene, inspect) answered
 * as {"ok":true,"result":{"text":...}} without mutating the document. */
char *vcad_agent_manifest(void);
char *vcad_engine_agent_read(const VcadEngine *engine, const char *tool, const char *args_json);


typedef struct VcadSolid VcadSolid;
typedef struct VcadMesh VcadMesh;
typedef struct VcadScene VcadScene;

typedef struct VcadMeshView {
  const float *vertices;
  size_t vertices_len; /* number of f32s (3 per vertex) */
  const float *normals;
  size_t normals_len;  /* 0 if absent/mismatched — synthesize on the caller side */
  const uint32_t *indices;
  size_t indices_len; /* number of u32s (3 per triangle) */
} VcadMeshView;

typedef struct VcadAabb {
  double min[3];
  double max[3];
} VcadAabb;

typedef struct VcadHit {
  uint8_t hit;
  double point[3];
  double normal[3];
  double t;
} VcadHit;

uint32_t vcad_ffi_abi_version(void);

VcadSolid *vcad_solid_cube(double sx, double sy, double sz);
VcadSolid *vcad_solid_cylinder(double radius, double height, uint32_t segments);
VcadSolid *vcad_solid_sphere(double radius, uint32_t segments);

VcadSolid *vcad_solid_fillet(const VcadSolid *solid, double radius);
VcadSolid *vcad_solid_chamfer(const VcadSolid *solid, double distance);
VcadAabb vcad_solid_bbox(const VcadSolid *solid);

VcadMesh *vcad_solid_to_mesh(const VcadSolid *solid, uint32_t segments);
VcadMeshView vcad_mesh_view(const VcadMesh *mesh);

void vcad_mesh_free(VcadMesh *mesh);
void vcad_solid_free(VcadSolid *solid);

VcadHit vcad_solid_raycast(const VcadSolid *solid, const double *origin, const double *dir);

/* Document evaluation: parse + evaluate a .vcad JSON document into a scene.
 * vcad_scene_from_loon compiles a loon source program (the AI-intent path) to
 * the same IR and evaluates it identically. */
VcadScene *vcad_scene_from_json(const uint8_t *json, size_t json_len);
/* Like vcad_scene_from_json, but resolves RELATIVE MeshImport paths against
 * base_dir (the document's own directory). A document that ships its meshes
 * alongside itself references them relatively; without a base directory they
 * resolve against the process working directory, so the same file renders from
 * one launch and comes back empty from another. NULL/empty base_dir behaves
 * exactly like vcad_scene_from_json. */
VcadScene *vcad_scene_from_json_in(const uint8_t *json, size_t json_len,
                                   const uint8_t *base_dir, size_t base_dir_len);

/* Incremental evaluation session: keep one per open document and evaluate
 * every edit through it. Only nodes whose subtree changed since the previous
 * evaluation are redone (and only changed roots re-tessellated); the scene
 * equals vcad_scene_from_json_in. Null on error (see vcad_last_error). */
typedef struct VcadEvalSession VcadEvalSession;
VcadEvalSession *vcad_eval_session_new(void);
void vcad_eval_session_free(VcadEvalSession *session);
void vcad_eval_session_clear(VcadEvalSession *session);
void vcad_eval_session_stats(const VcadEvalSession *session, size_t *reused_solids,
                             size_t *evaluated_solids, size_t *reused_meshes);
VcadScene *vcad_scene_from_json_session(VcadEvalSession *session,
                                        const uint8_t *json, size_t json_len,
                                        const uint8_t *base_dir, size_t base_dir_len);

VcadScene *vcad_scene_from_loon(const uint8_t *loon, size_t loon_len);
/* Compile a .loon FILE (path bytes) to .vcad document JSON: [use] modules and
 * relative import-mesh paths resolve against the file's directory (mesh paths
 * come back absolute), so the JSON evaluates like any .vcad document. Owned
 * string, free with vcad_cam_free; NULL + vcad_last_error on failure. */
char *vcad_loon_file_to_json(const uint8_t *path, size_t path_len);
/* Same, but on failure (NULL) stores an owned JSON diagnostic in *out_diag:
 * {"message","file","line","column"} (line/column 1-based, null when the
 * error can't be attributed to the file). Free both with vcad_cam_free. */
char *vcad_loon_file_to_json_diag(const uint8_t *path, size_t path_len, char **out_diag);
/* Parameter sensitivities of an IR document: per named parameter and solid
 * part, d(volume, mass, centroid)/dθ from the differentiable seam. Masses in
 * grams at density_kg_m3. Owned JSON (free with vcad_cam_free); null when the
 * input isn't a document. */
char *vcad_document_sensitivity(const uint8_t *json, size_t json_len, double density_kg_m3);
/* Any .vcad format (JSON, loon source, VCode) -> IR document JSON, mesh paths
 * resolved against the file's directory. Free with vcad_cam_free; null on error. */
char *vcad_document_file_to_json(const uint8_t *path, size_t path_len);
size_t vcad_scene_part_count(const VcadScene *scene);
VcadMeshView vcad_scene_part_mesh(const VcadScene *scene, size_t index);

/* Progressive evaluation. vcad_eval_begin parses `json` (mesh paths resolved
 * against base_dir, may be NULL) and starts the kernel walk on its own
 * thread; NULL if the document fails to parse (read vcad_last_error).
 * vcad_eval_progress writes visible roots landed / total and returns true
 * once the walk has finished. vcad_eval_part_mesh borrows a landed root's mesh
 * (empty view until it lands; valid until finish/abandon). vcad_eval_finish
 * joins and returns the scene (NULL + vcad_last_error on failure), freeing the
 * job. vcad_eval_abandon frees the job without waiting. One caller per job. */
typedef struct VcadEvalJob VcadEvalJob;
VcadEvalJob *vcad_eval_begin(const uint8_t *json, size_t json_len,
                             const uint8_t *base_dir, size_t base_dir_len);
/* vcad_eval_begin at a level of detail: preview = true tessellates for a quick
 * first draw (no export repair, no chord tolerance); follow it with a full
 * job and swap the scene in when that lands. */
VcadEvalJob *vcad_eval_begin_lod(const uint8_t *json, size_t json_len,
                                 const uint8_t *base_dir, size_t base_dir_len,
                                 bool preview);
bool vcad_eval_progress(const VcadEvalJob *job, size_t *done, size_t *total);
VcadMeshView vcad_eval_part_mesh(const VcadEvalJob *job, size_t index);
VcadScene *vcad_eval_finish(VcadEvalJob *job);
void vcad_eval_abandon(VcadEvalJob *job);

/* Mesh bundles: a document's solved root meshes, keyed as the root cache keys
 * them, so a file ships with its geometry and opens without a kernel walk.
 * Keys are per-part root-cache keys (owned string, free with vcad_cam_free;
 * NULL when the part is not cacheable). vcad_mesh_bundle_write takes
 * newline-separated keys and pulls the meshes from the cache; import feeds a
 * bundle into the cache. Both return the number of entries handled. */
char *vcad_scene_root_key(const VcadScene *scene, size_t index);
size_t vcad_scene_write_mesh_bundle(const VcadScene *scene, const uint8_t *path, size_t path_len);
size_t vcad_mesh_bundle_write(const uint8_t *keys, size_t keys_len,
                              const uint8_t *path, size_t path_len);
size_t vcad_mesh_bundle_import(const uint8_t *path, size_t path_len);
void vcad_scene_free(VcadScene *scene);

/* Assembly instances + kinematic joint playback. Assembly documents place
 * geometry via partDefs + instances + joints (joints fully place children).
 * Instance meshes are part-def-LOCAL; transforms come separately so playback
 * can drive them per frame. When instance_count > 0 render instances INSTEAD
 * of the root parts (assembly docs may carry both). Id/material pointers are
 * UTF-8, NOT NUL-terminated (length via out_len), valid for the scene's
 * lifetime. Transforms are 16 doubles, COLUMN-major (out[col*4+row]).
 * vcad_scene_solve_fk takes a JSON object {"jointId": value, ...} (degrees /
 * mm; joint name accepted as fallback), runs the kernel forward-kinematics
 * solver, and writes instance_count 4x4 world matrices in instance-index
 * order into out (capacity out_cap doubles). Returns instances written, 0 on
 * error. Only scenes from vcad_scene_from_json/_loon support FK. */
size_t vcad_scene_instance_count(const VcadScene *scene);
VcadMeshView vcad_scene_instance_mesh(const VcadScene *scene, size_t index);
const uint8_t *vcad_scene_instance_id(const VcadScene *scene, size_t index, size_t *out_len);
const uint8_t *vcad_scene_instance_material(const VcadScene *scene, size_t index, size_t *out_len);
uint8_t vcad_scene_instance_transform(const VcadScene *scene, size_t index, double *out);
size_t vcad_scene_solve_fk(const VcadScene *scene, const uint8_t *joints_json,
                           size_t json_len, double *out, size_t out_cap);

/* Feature edges (boundary + creases sharper than angle_deg) for the
 * wireframe/edge overlay. 6 floats per segment: ax ay az bx by bz. */
typedef struct VcadEdges VcadEdges;
typedef struct VcadEdgesView {
  const float *floats;
  size_t floats_len;
} VcadEdgesView;
VcadEdges *vcad_scene_part_edges(const VcadScene *scene, size_t index, float angle_deg);
VcadEdges *vcad_scene_instance_edges(const VcadScene *scene, size_t index, float angle_deg);
const uint8_t *vcad_scene_instance_part_def(const VcadScene *scene, size_t index, size_t *out_len);
VcadEdges *vcad_mesh_edges(const VcadMesh *mesh, float angle_deg);
VcadEdgesView vcad_edges_view(const VcadEdges *edges);
void vcad_edges_free(VcadEdges *edges);

/* Per-triangle FACE ids for a scene part: one u32 per triangle of
 * vcad_scene_part_mesh, naming which face of the solid produced it (ordinal
 * within the shell). UINT32_MAX marks a triangle with no face (bridging fill).
 * Writes the count to out_len; returns NULL when the part carries no tags
 * (e.g. a frozen or imported mesh with no B-rep), so face picking is an
 * optional capability. Borrows the scene; valid until the scene is freed.
 * NOT durable across edits — any change to the B-rep renumbers the ordinals. */
const uint32_t *vcad_scene_part_face_ids(const VcadScene *scene, size_t index,
                                         size_t *out_len);

/* Direct BRep ray tracing (pixel-perfect mode): rays hit analytic surfaces,
 * no tessellation. Kernel coords (Z-up, mm); colors = 3 f32 per part
 * (linear RGB); RGBA8 row-major output. CPU renderer today, signature is
 * renderer-agnostic (Metal/wgpu can swap in behind it). */
typedef struct VcadImage VcadImage;
typedef struct VcadImageView {
  const uint8_t *pixels;
  size_t pixels_len;
  uint32_t width;
  uint32_t height;
} VcadImageView;
VcadImage *vcad_scene_raytrace(const VcadScene *scene, const double *cam,
                               const double *target, double fov_deg,
                               uint32_t width, uint32_t height,
                               const float *colors, size_t colors_len);
VcadImage *vcad_scene_raytrace_gpu(const VcadScene *scene, const double *cam,
                                   const double *target, double fov_deg,
                                   uint32_t width, uint32_t height,
                                   const float *colors, size_t colors_len);
VcadImageView vcad_image_view(const VcadImage *image);
void vcad_image_free(VcadImage *image);

/* Resident parametric document — set one parameter, re-solve every bound domain.
 * vcad_doc_gripper_slice1 builds the cross-domain worked example (one connector_x
 * drives an enclosure cutout + a board connector). */
typedef struct VcadDoc VcadDoc;
VcadDoc *vcad_doc_load(const uint8_t *json, size_t json_len);
VcadDoc *vcad_doc_gripper_slice1(void);
VcadScene *vcad_doc_set_param(VcadDoc *doc, const char *name, double value);
/* Per-frame path: re-solve only cheap roots (skips the sheet-metal fold), so the
 * mechanical cutout follows the drag live while the fold waits for settle
 * (vcad_doc_set_param). Same ownership. */
VcadScene *vcad_doc_set_param_cheap(VcadDoc *doc, const char *name, double value);
/* Per-frame REAL min-wall (mm): resolve the doc at its current params and measure
 * the enclosure's thinnest enclosed-face clearance to the connector cutout from
 * geometry — cheap enough to run live (no fold, no routing). f64::INFINITY when
 * unmeasurable. The −Y front is the connector port and is excluded. */
double vcad_doc_min_wall(const VcadDoc *doc);
void vcad_doc_free(VcadDoc *doc);

/* Slice 2 — copper re-route. vcad_route_traces builds the tiny 2-net gripper
 * board at connector_x (mm, board-local) and routes it with the OHM auto-router,
 * returning copper segments to draw as the connector moves. layer: 0=FCu 1=BCu
 * 2+=inner; net_id: 0=SIG 1=GND. Coords are board-local mm (same frame as the
 * board plate). Caller owns the result (vcad_route_result_free). */
typedef struct VcadRouteResult VcadRouteResult;
typedef struct VcadTraceLine {
  double start[2];
  double end[2];
  double width;
  uint32_t layer;
  uint32_t net_id;
} VcadTraceLine;
VcadRouteResult *vcad_route_traces(double connector_x, double width);
size_t vcad_route_result_trace_count(const VcadRouteResult *r);
VcadTraceLine vcad_route_result_trace(const VcadRouteResult *r, size_t idx);
size_t vcad_route_result_unrouted_count(const VcadRouteResult *r);
void vcad_route_result_free(VcadRouteResult *r);

/* The unified cross-domain solve. vcad_doc_solve sets connector_x and returns
 * EVERY domain at once — meshes (enclosure + board + sheet-metal bracket), copper
 * traces, and the receipt scalars — all from the one resolved parameter. The
 * SETTLE path (full + expensive); per-frame still uses vcad_doc_set_param_cheap.
 * Caller owns the result (vcad_solve_free). */
typedef struct VcadSolve VcadSolve;
VcadSolve *vcad_doc_solve(VcadDoc *doc, const char *name, double value);
size_t vcad_solve_part_count(const VcadSolve *s);
VcadMeshView vcad_solve_part_mesh(const VcadSolve *s, size_t index);
size_t vcad_solve_trace_count(const VcadSolve *s);
VcadTraceLine vcad_solve_trace(const VcadSolve *s, size_t idx);
size_t vcad_solve_unrouted(const VcadSolve *s);  /* receipt: nets unrouted (0 = ok) */
double vcad_solve_min_wall(const VcadSolve *s);   /* receipt: connector-wall mm */
/* Receipt verdicts (ABI 4). bracket_ok: 1=Held, 0=Violated (DFM Error).
 * bracket_severity: 0 clean / 1 Warning / 2 Error. quote_cost_cents: integer
 * cents TOTAL (enclosure + board + bracket, qty 1). lead_days: HEURISTIC.
 * all_held: AND of gating domains (min-wall, copper, bracket; quote never
 * gates) — gate the Make-it button. */
uint8_t vcad_solve_bracket_ok(const VcadSolve *s);
uint8_t vcad_solve_bracket_severity(const VcadSolve *s);
uint64_t vcad_solve_quote_cost_cents(const VcadSolve *s);
uint32_t vcad_solve_lead_days(const VcadSolve *s);
uint8_t vcad_solve_all_held(const VcadSolve *s);
/* Per-domain quote breakdown (ABI 5): enclosure CNC + bracket fold are
 * kernel-real cost models (removed-volume / unfold); board is a labeled estimate
 * (no Rust PCB cost model). quote_has_estimate = 1 when the total includes the
 * labeled board line, so the UI tags it "est." Sum == quote_cost_cents. */
uint64_t vcad_solve_quote_enclosure_cents(const VcadSolve *s);
uint64_t vcad_solve_quote_board_cents(const VcadSolve *s);
uint64_t vcad_solve_quote_bracket_cents(const VcadSolve *s);
uint8_t vcad_solve_quote_has_estimate(const VcadSolve *s);
void vcad_solve_free(VcadSolve *s);

/* =====================================================================
 * Simulation (ABI 6) — physics envs, the render seam, policy inference,
 * and in-process training. See crates/vcad-ffi/src/gym.rs and train.rs.
 *
 * UNITS. Body transforms come back in MILLIMETERS, column-major, exactly
 * like vcad_scene_solve_fk — a renderer that draws kinematic playback draws
 * a physics rollout unchanged. Observations are NOT rescaled: they are the
 * policy's input space (degrees / mm / meters as documented per field) and
 * must stay bit-identical to what training saw.
 *
 * ERRORS. Every entry point below returns NULL/0 on failure AND records a
 * human-readable reason retrievable with vcad_last_error() on the SAME
 * thread. Read it immediately after a failed call.
 * ===================================================================== */

/* Borrow this thread's last error as UTF-8 (NOT null-terminated; use the
 * length). Returns NULL when no error is recorded. Valid until the next FFI
 * call on this thread. */
const uint8_t *vcad_last_error(size_t *out_len);

typedef struct VcadGym VcadGym;
typedef struct VcadPolicy VcadPolicy;
typedef struct VcadTrainer VcadTrainer;

/* A borrowed view of the most recent step/reset. Every pointer borrows from
 * the owning gym and is invalidated by the next step/reset on it — copy out
 * before stepping again. Lengths are element counts.
 *
 * base_height_m / base_tilt_deg are NOISE-FREE ground truth; base_pose is the
 * (possibly noisy) sensor view the policy trains against. They deliberately
 * disagree when observation noise is configured. Reward and termination read
 * the former; the policy reads the latter. */
typedef struct VcadGymStepView {
  const double *joint_positions;      /* degrees (rotational) / mm (linear) */
  size_t joint_positions_len;
  const double *joint_velocities;     /* per second, same per-DOF units */
  size_t joint_velocities_len;
  const double *end_effector_poses;   /* 7 per EE: x,y,z (m), qw,qx,qy,qz */
  size_t end_effector_poses_len;
  const double *end_effector_contacts;/* 5 per EE: touch(0/1), N, cop xyz */
  size_t end_effector_contacts_len;
  const double *base_pose;            /* 7 doubles, or NULL */
  const double *base_velocity;        /* 6 doubles (v xyz m/s, w xyz rad/s) */
  double reward;                      /* always 0 — compute task reward yourself */
  uint8_t done;
  uint8_t terminated;
  uint8_t truncated;
  uint8_t has_base;
  uint32_t step;
  uint32_t action_latency_substeps;
  double base_height_m;               /* ground truth */
  double base_tilt_deg;               /* ground truth */
  const uint8_t *termination_reason;  /* UTF-8, or NULL */
  size_t termination_reason_len;
} VcadGymStepView;

/* Create an env from a .vcad JSON document and a GymSpec JSON blob. Pass
 * spec_json = NULL (len 0) for defaults. Returns NULL on failure. */
VcadGym *vcad_gym_create(const uint8_t *doc_json, size_t doc_json_len,
                         const uint8_t *spec_json, size_t spec_json_len);
void vcad_gym_free(VcadGym *gym);

/* Reset, drawing domain randomization from `seed`. Returns 1 on success. */
uint8_t vcad_gym_reset(VcadGym *gym, uint64_t seed);

/* Step once. action_kind: 0 = torque (Nm/N), 1 = position target (deg/mm),
 * 2 = velocity target (deg/s, mm/s). actions_len MUST equal
 * vcad_gym_action_dim — a mismatch is refused, never padded. */
uint8_t vcad_gym_step(VcadGym *gym, const double *actions, size_t actions_len,
                      uint32_t action_kind);

/* Refresh the view from true state without advancing time. */
uint8_t vcad_gym_observe(VcadGym *gym);
VcadGymStepView vcad_gym_step_view(const VcadGym *gym);

/* Introspection. */
size_t vcad_gym_action_dim(const VcadGym *gym);       /* action vector length */
size_t vcad_gym_obs_dim(const VcadGym *gym);          /* POLICY feature count */
size_t vcad_gym_observation_dim(const VcadGym *gym);  /* raw observation slots */
double vcad_gym_control_dt(const VcadGym *gym);       /* dt*substeps, seconds */
uint32_t vcad_gym_max_steps(const VcadGym *gym);
size_t vcad_gym_actuated_joint_count(const VcadGym *gym);
const uint8_t *vcad_gym_actuated_joint_id(const VcadGym *gym, size_t index,
                                          size_t *out_len);
size_t vcad_gym_body_count(const VcadGym *gym);
const uint8_t *vcad_gym_body_id(const VcadGym *gym, size_t index, size_t *out_len);

/* Render seam. Writes 16 doubles per body, COLUMN-MAJOR, MILLIMETERS —
 * identical layout to vcad_scene_solve_fk. out_cap is in doubles. */
size_t vcad_gym_body_transforms(const VcadGym *gym, double *out, size_t out_cap);

/* Bind the env's bodies to a scene's instance ordering (call once, after
 * creating both from the same document). Returns the number of scene
 * instances that matched a simulated body. */
size_t vcad_gym_bind_scene(VcadGym *gym, const VcadScene *scene);
size_t vcad_gym_scene_binding_len(const VcadGym *gym);
/* Transforms in SCENE INSTANCE ORDER — the index space the vcad_scene_instance_*
 * calls use. Unmatched instances are left UNTOUCHED, so pre-fill `out` with
 * authored transforms to keep static scenery. Requires vcad_gym_bind_scene. */
size_t vcad_gym_scene_transforms(const VcadGym *gym, double *out, size_t out_cap);

/* Shove the floating base: angular (rad/s) then BODY-FRAME linear (m/s),
 * added to its current velocity. Returns 0 if there is no floating base. */
uint8_t vcad_gym_nudge_base(VcadGym *gym, double dwx, double dwy, double dwz,
                            double dvx, double dvy, double dvz);

/* Write the current policy feature vector (for plotting). Returns the count. */
size_t vcad_gym_features(VcadGym *gym, double *out, size_t out_cap);

/* --- Policy inference -------------------------------------------------
 * Inference lives in Rust so the forward pass matches training EXACTLY
 * (whitening, output clamp, default-pose offset). Do not reimplement it in
 * Swift: a drift of one clamp gives a robot that almost stands. */
VcadPolicy *vcad_policy_load(const uint8_t *json, size_t json_len);
/* Load a .vcadpolicy bundle. document_json may be NULL to skip the drift
 * check; when supplied and the hash differs the policy STILL loads (staleness
 * is a judgement, not a load error) and vcad_last_error describes the drift. */
VcadPolicy *vcad_policy_load_bundle(const uint8_t *bundle_json, size_t bundle_json_len,
                                    const uint8_t *document_json, size_t document_json_len);
void vcad_policy_free(VcadPolicy *policy);
size_t vcad_policy_obs_dim(const VcadPolicy *policy);
size_t vcad_policy_act_dim(const VcadPolicy *policy);
uint8_t vcad_policy_is_mlp(const VcadPolicy *policy);
/* Dimensional compatibility check with a descriptive error. Call at load. */
uint8_t vcad_policy_check(const VcadGym *gym, const VcadPolicy *policy);
/* A zero (hold-rest-pose) policy matched to this env — the baseline. */
VcadPolicy *vcad_policy_zeros(const VcadGym *gym, double action_scale_deg);
/* Step by evaluating the policy: features -> act -> position targets. */
uint8_t vcad_gym_policy_step(VcadGym *gym, const VcadPolicy *policy);
/* The action the last policy step issued (joint targets, degrees), or NULL. */
const double *vcad_gym_last_action(const VcadGym *gym, size_t *out_len);

/* --- Reward and provenance -------------------------------------------- */
/* Evaluate a RewardSpec JSON against the gym's most recent step. Returns 0
 * before the first step of an episode, NaN on failure. */
double vcad_gym_reward(const VcadGym *gym, const uint8_t *reward_json,
                       size_t reward_json_len);
/* Content hash of a document ("fnv1a64:xxxxxxxxxxxxxxxx", 23 bytes). */
size_t vcad_document_hash(const uint8_t *doc_json, size_t doc_json_len,
                          uint8_t *out, size_t out_cap);

/* --- Training ---------------------------------------------------------- */
typedef struct VcadTrainProgress {
  uint32_t iteration;
  uint32_t total_iterations;
  double mean_reward;
  double eval_reward;   /* trainer's own eval — NOT trustworthy, see docs */
  uint32_t eval_steps;
  double sigma;         /* top-k spread; collapsing toward 0 precedes divergence */
  double update_norm;
  double step_size;
  double best_held_out; /* the only number a run may be judged by */
  uint32_t best_held_out_full;
  uint32_t best_iteration;
  uint8_t running;
  uint8_t finished;
  uint8_t failed;
  uint8_t cancelled;
} VcadTrainProgress;

/* Start a run on a worker thread; returns immediately. train_spec_json and
 * reward_json may be NULL for defaults. gym_spec_json uses the SAME GymSpec
 * shape as vcad_gym_create, so the simulated env and the trained env cannot
 * disagree. */
VcadTrainer *vcad_train_start(const uint8_t *doc_json, size_t doc_json_len,
                              const uint8_t *gym_spec_json, size_t gym_spec_json_len,
                              const uint8_t *train_spec_json, size_t train_spec_json_len,
                              const uint8_t *reward_json, size_t reward_json_len);
uint8_t vcad_train_poll(const VcadTrainer *trainer, VcadTrainProgress *out);
void vcad_train_stop(VcadTrainer *trainer);
/* Two-call protocol. Sizing call: out=NULL (or out_cap=0) RETURNS the number of
 * bytes required, or 0 if no policy has been scored yet. Copy call: pass a
 * buffer of at least that size; returns bytes written, or 0 if too small. */
size_t vcad_train_best_policy_json(const VcadTrainer *trainer, uint8_t *out, size_t out_cap);
const uint8_t *vcad_train_error(const VcadTrainer *trainer, size_t *out_len);
/* Cancels, JOINS the worker, then frees. Blocks — deliberately. */
void vcad_train_free(VcadTrainer *trainer);

/* File > Export. Same writers as `vcad export`: STL (roots + posed instances,
 * one soup), STEP (analytic B-rep; mesh-only parts refused by name), GLB (one
 * named mesh per part). base_dir (nullable) resolves relative mesh paths.
 * Writes atomically. Returns parts written (>= 1), or -1 + vcad_last_error. */
#define VCAD_EXPORT_STL 0u
#define VCAD_EXPORT_STEP 1u
#define VCAD_EXPORT_GLB 2u
int32_t vcad_export_document(const uint8_t *json, size_t json_len,
                             const uint8_t *base_dir, size_t base_dir_len,
                             const uint8_t *out_path, size_t out_path_len,
                             uint32_t format);

/* Owned byte buffer (e.g. an encoded PNG). */
typedef struct VcadBytes VcadBytes;
typedef struct VcadBytesView {
    const uint8_t *data;
    size_t len;
} VcadBytesView;
VcadBytesView vcad_bytes_view(const VcadBytes *bytes);
void vcad_bytes_free(VcadBytes *bytes);

/* File > Render Photoreal: the `vcad render --photoreal` renderer, linked in.
 * GPU tracer, CPU fallback when no adapter (or force_cpu). Camera in the
 * kernel Z-up frame: azimuth CCW from +X, elevation above XY, degrees. */
typedef struct VcadPhotorealParams {
    uint32_t width;
    uint32_t height;
    uint32_t samples;
    double azimuth_deg;
    double elevation_deg;
    uint8_t force_cpu;
} VcadPhotorealParams;
/* Called from the rendering thread after each sample batch; return non-zero
 * to cancel. */
typedef uint8_t (*VcadRenderProgress)(void *ctx, uint32_t samples_done, uint32_t samples_total);
/* Blocking — call off the main thread, and read vcad_last_error on that same
 * thread. Returns an owned PNG, or NULL ("cancelled" after a cancel). */
VcadBytes *vcad_render_photoreal_png(const uint8_t *json, size_t json_len,
                                     const uint8_t *base_dir, size_t base_dir_len,
                                     const VcadPhotorealParams *params,
                                     VcadRenderProgress progress, void *ctx);

/* ===================== Modeling surface (ABI 7) =====================
 * Owned strings from vcad_sketch_* / vcad_topology_* / vcad_feature_* /
 * vcad_import_* are freed ONLY with vcad_string_free. Failures return
 * NULL / 0 / -1 and record vcad_last_error (same thread). */
void vcad_string_free(char *s);

/* --- Sketch constraint solver (crates/vcad-ffi/src/sketch.rs) ---
 * Entity handles are 1-based uint32 (0 = error). Point selectors: */
#define VCAD_SKETCH_REF_SELF 0u
#define VCAD_SKETCH_REF_START 1u
#define VCAD_SKETCH_REF_END 2u
#define VCAD_SKETCH_REF_CENTER 3u
/* Constraint kinds (field usage documented on sketch::kind). */
#define VCAD_SC_COINCIDENT 0u
#define VCAD_SC_HORIZONTAL 1u
#define VCAD_SC_VERTICAL 2u
#define VCAD_SC_PARALLEL 3u
#define VCAD_SC_PERPENDICULAR 4u
#define VCAD_SC_TANGENT 5u
#define VCAD_SC_DISTANCE 6u
#define VCAD_SC_LENGTH 7u
#define VCAD_SC_RADIUS 8u
#define VCAD_SC_ANGLE 9u /* degrees */
#define VCAD_SC_EQUAL_LENGTH 10u
#define VCAD_SC_EQUAL_RADIUS 11u
#define VCAD_SC_FIXED 12u /* value = x, value2 = y */
#define VCAD_SC_POINT_ON_LINE 13u
#define VCAD_SC_CONCENTRIC 14u
#define VCAD_SC_POINT_ON_CIRCLE 15u
#define VCAD_SC_MIDPOINT 16u
#define VCAD_SC_HORIZONTAL_DISTANCE 17u
#define VCAD_SC_VERTICAL_DISTANCE 18u
#define VCAD_SC_DIAMETER 19u
#define VCAD_SC_POINT_LINE_DISTANCE 20u
#define VCAD_SC_SYMMETRIC 21u
typedef struct VcadSketch VcadSketch;
typedef struct VcadSketchConstraint {
    uint32_t kind;
    uint32_t e0;
    uint32_t r0;
    uint32_t e1;
    uint32_t r1;
    uint32_t e2;
    uint32_t r2;
    double value;
    double value2;
} VcadSketchConstraint;
#define VCAD_SKETCH_UNDER 0
#define VCAD_SKETCH_FULL 1
#define VCAD_SKETCH_OVER 2
typedef struct VcadSketchSolve {
    uint8_t converged;
    uint8_t state;  /* VCAD_SKETCH_UNDER / _FULL / _OVER */
    uint8_t status; /* 0 converged,1 max iter,2 lambda overflow,3 no constraints,4 no params,5 singular,255 bad handle */
    int32_t dof;
    uint32_t iterations;
    double residual;
} VcadSketchSolve;
VcadSketch *vcad_sketch_new(void);
VcadSketch *vcad_sketch_new_on_plane(const double *origin, const double *x_dir, const double *y_dir);
void vcad_sketch_free(VcadSketch *s);
uint32_t vcad_sketch_add_point(VcadSketch *s, double x, double y);
uint32_t vcad_sketch_add_line(VcadSketch *s, uint32_t start, uint32_t end);
uint32_t vcad_sketch_add_arc(VcadSketch *s, uint32_t start, uint32_t end, uint32_t center, bool ccw);
uint32_t vcad_sketch_add_circle(VcadSketch *s, uint32_t center, double radius);
/* Returns the constraint index (>= 0) or -1. */
int32_t vcad_sketch_add_constraint(VcadSketch *s, const VcadSketchConstraint *c);
/* Edit a driving dimension in place (ANGLE in degrees), then re-solve. */
bool vcad_sketch_set_dimension(VcadSketch *s, uint32_t index, double value, double value2);
bool vcad_sketch_pop_constraint(VcadSketch *s);
int32_t vcad_sketch_dof(const VcadSketch *s);
VcadSketchSolve vcad_sketch_solve(VcadSketch *s);
/* Solved 2D position of a point / line|arc start|end / center. */
bool vcad_sketch_point(const VcadSketch *s, uint32_t handle, uint32_t which, double *out_xy);
double vcad_sketch_radius(const VcadSketch *s, uint32_t handle);
/* Closed profile as JSON {origin,x_dir,y_dir,segments:[IR SketchSegment2D]}. */
char *vcad_sketch_profile_json(const VcadSketch *s);

/* --- Topology picking + selection modifiers (topology.rs) ---
 * Face id = ordinal in the outer shell (matches vcad_scene_part_face_ids);
 * edge id = ordinal from vcad_topology_json. Session handles, not durable. */
#define VCAD_TOPO_NONE 0xFFFFFFFFu
VcadSolid *vcad_scene_part_solid(const VcadScene *scene, size_t index);
/* {"faces":[{id,surface,center,normal,area,selector}],
 *  "edges":[{id,faces:[a,b],a,b,segments:[f32 x6..],planar,selector}]} */
char *vcad_topology_json(const VcadSolid *solid);
typedef struct VcadPick {
    uint8_t hit;
    uint32_t face;
    uint32_t edge;
    double point[3];
    double normal[3];
    double t;
    double edge_distance;
} VcadPick;
VcadPick vcad_solid_pick(const VcadSolid *solid, const double *origin, const double *dir, double edge_tol);
VcadSolid *vcad_solid_fillet_edges(const VcadSolid *solid, const uint32_t *edge_ids, size_t count, double radius);
VcadSolid *vcad_solid_chamfer_edges(const VcadSolid *solid, const uint32_t *edge_ids, size_t count, double distance);
VcadSolid *vcad_solid_shell_open(const VcadSolid *solid, double thickness, const uint32_t *face_ids, size_t count);

/* --- Feature builders (features.rs): append IR nodes to a .vcad document.
 * feature_json {"kind": extrude|cut_extrude|extrude_to_face|revolve|sweep|
 * loft|linear_pattern|circular_pattern|mirror|scale|text|cone|edge_blend|
 * shell|boolean, ...}. doc_json NULL/"" = new document. Returns
 * {"document":..., "node_id":N, "added":[..]}. */
char *vcad_feature_append(const char *doc_json, const char *feature_json);

/* --- Import (import.rs) --- */
/* {"document": <StepImport nodes, one root per solid>, "solids": N} */
char *vcad_import_step(const char *path);
/* Cancellable STEP import job: start -> poll (stage, done of total) ->
 * [cancel] -> finish (joins the thread, frees the job, returns the same JSON
 * as vcad_import_step plus "skipped_faces" and "warnings"; null +
 * last error "import step: cancelled" after a cancel). Always finish. */
typedef struct VcadStepImportJob VcadStepImportJob;
#define VCAD_STEP_STAGE_READING 0
#define VCAD_STEP_STAGE_PARSING 1
#define VCAD_STEP_STAGE_FACES 2
#define VCAD_STEP_STAGE_DONE 3
VcadStepImportJob *vcad_import_step_start(const char *path);
bool vcad_import_step_poll(const VcadStepImportJob *job, uint32_t *stage,
                           size_t *done, size_t *total);
void vcad_import_step_cancel(const VcadStepImportJob *job);
char *vcad_import_step_finish(VcadStepImportJob *job);
/* IR assembly document JSON. */
char *vcad_import_urdf(const char *path);

/* --- Image to CAD, EXPERIMENTAL (recon.rs, feature "recon", via irys-recon).
 * request_json {"photos": [paths], "tag_mm": 50, "sensor_w_mm": null}; every
 * photo must show a tag36h11 AprilTag and carry an EXIF focal length.
 * vcad_recon_check: fast pre-flight -> [{name, size, tag_id, rms_px,
 * focal_px, focal_exif_px}]. vcad_recon_photos: the reconstruction (minutes,
 * blocking, not interruptible; call off the main thread) -> {"document",
 * "program", "dimensions": [{name, parameter, value, sigma, unit}], "weak",
 * "low_confidence", "warnings", "photos", "seconds"}. Null + vcad_last_error
 * (a plain, user-facing reason) on failure. Free with vcad_string_free. */
char *vcad_recon_check(const char *request_json);
char *vcad_recon_photos(const char *request_json);

/* Cancellable reconstruction job. vcad_recon_start (same request) -> handle,
 * or NULL + vcad_last_error for a malformed request. vcad_recon_poll copies
 * progress (any thread, non-blocking): returns 1 done, 0 running, -1 null.
 * stage: 0 starting, 1 calibrate, 2 appearance, 3 search, 4 finalists,
 * 5 polish; step is 1-based of `of`. vcad_recon_cancel (any thread) stops it
 * within one LM iteration. vcad_recon_finish blocks (call off the main
 * thread) -> the vcad_recon_photos JSON, or NULL + vcad_last_error, which is
 * exactly "cancelled" after a cancel; free with vcad_string_free.
 * vcad_recon_job_free cancels, joins and frees. */
typedef struct VcadReconJob VcadReconJob;
typedef struct {
    int32_t stage;
    uint32_t step;
    uint32_t of;
    uint8_t done;
    uint8_t cancelled;
} VcadReconProgress;
VcadReconJob *vcad_recon_start(const char *request_json);
int32_t vcad_recon_poll(const VcadReconJob *job, VcadReconProgress *out);
void vcad_recon_cancel(const VcadReconJob *job);
char *vcad_recon_finish(VcadReconJob *job);
void vcad_recon_job_free(VcadReconJob *job);

/* --- Drawings (drafting.rs): 2D shop drawings on vcad-kernel-drafting.
 * request_json (NULL/"" = defaults): {"view": front|back|top|bottom|left|
 * right|isometric, "show_hidden": bool, "show_dimensions": bool,
 * "details": [{"label","center":[x,y],"scale","width","height"}],
 * "gdt": [{"kind": flatness|position|perpendicularity|datum, "tolerance",
 * "datum", "at":[x,y]}], "drawing": <IR DrawingSettings override>,
 * "base_dir": str}. Section lines, title block and BOM come from the
 * document's `drawing` settings. */
#define VCAD_DRAWING_DXF 0
#define VCAD_DRAWING_PDF 1

/**
 * DXF of the whole composed sheet in model mm (views at true size).
 */
#define VCAD_DRAWING_SHEET_DXF 2
/* Composed A4 sheet: {"width","height","sheet":{lines,arcs,polygons,texts},
 * "main":{sheet_center,view_center,scale,scale_note,bounds},
 * "visible_edges","hidden_edges","dxf_entities","sections"}. Free with
 * vcad_string_free; NULL + vcad_last_error on failure. */
char *vcad_drawing_sheet_json(const char *doc_json, const char *request_json);
/* DXF (active view, same writer as the web export) or PDF (the sheet), written
 * atomically. Returns DXF entity count / PDF bytes, or -1 + vcad_last_error. */
int64_t vcad_drawing_export(const char *doc_json, const char *request_json,
                            const char *out_path, uint32_t format);
/* --- Analysis: DFM / FEA / clearance (dfm.rs, fea.rs, clearance.rs) ---
 * All take NUL-terminated IR JSON, return owned JSON freed ONLY with
 * vcad_string_free; null + vcad_last_error on failure (fail-closed). */
/* request {"process":"fdm"|"cnc_3axis"|..., "rule_pack_toml"?, "ruleset"?} ->
 * {issues:[{...kernel issue, part_index, face_ids, patch|null, patch_note}], counts, score, ...} */
char *vcad_dfm_check(const char *doc_json, const char *request_json);
/* patch = an issue's "patch" object -> patched IR document JSON. */
char *vcad_dfm_apply_patch(const char *doc_json, const char *patch_json);
/* {"processes":[...], "rulesets":[...]} */
char *vcad_dfm_processes(void);
/* request {"part_index","material"?,"elastic"?,"resolution"?,"levels"?,
 * "loads":[{"faces":[ids]|"region":{min,max},"force":[N,N,N]}],
 * "supports":[{"faces"|"region","fix"?:[b,b,b]}],"fields"?} ->
 * {max_von_mises_mpa, max_displacement_mm, safety_factor, converged, study,
 *  field:{dims_zyx, origin_mm, voxel_mm, von_mises_mpa, displacement_mm, mask, ...}} */
char *vcad_fea_solve(const char *doc_json, const char *request_json);
/* vcad_fea_solve with a per-step hook (fraction 0..1, level, levels, solving);
 * the hook returns false to cancel -> NULL, vcad_last_error "FEA study cancelled". */
typedef bool (*vcad_fea_progress_fn)(void *ctx, double fraction, uint32_t level,
                                     uint32_t levels, bool solving);
char *vcad_fea_solve_progress(const char *doc_json, const char *request_json,
                              vcad_fea_progress_fn progress, void *ctx);
/* [{id,label,youngs_modulus_mpa,poisson,yield_strength_mpa}] */
char *vcad_fea_materials(void);
/* request (nullable) {"pairs"?:[[a,b]], "include_mesh"?} ->
 * {pairs:[{a,b,distance_mm,intersecting,overlap_volume_mm3,status,clash?}], ...} */
char *vcad_clearance(const char *doc_json, const char *request_json);

/* --- Live receipts (receipts.rs) ---
 * Re-verify every persisted clearance assertion (doc.clearance_specs) ->
 * {status:"Holds"|"Stale"|"Violated"|null, checks:[{label,claim_id,status,
 *  subject,required_mm,measured_mm?,baseline_mm?,worst_pair?,points?,reason?}],
 *  receipt: DesignReceipt} */
char *vcad_receipts_verify(const char *doc_json);
/* request (nullable) {"label"?} -> {document, accepted}: re-baseline the
 * assertions that currently hold (a Violated one is never accepted). */
char *vcad_receipts_accept(const char *doc_json, const char *request_json);

/* --- Assembly + animation authoring (assembly.rs, animation.rs) ---
 * Owned JSON freed ONLY with vcad_string_free; null + vcad_last_error on
 * failure. Additive, no ABI bump. */
/* .vcad JSON -> [{id,kind,instances:[a,b],pass,measured,expected,tolerance,
 * unit,detail,error|null}] — mates re-verified against the FK-posed assembly. */
char *vcad_assembly_check_mates(const char *doc_json);
/* Timeline JSON -> canonical Timeline JSON (keys sorted + clamped to
 * [0,durationS], coincident keys collapsed last-wins, empty tracks dropped). */
char *vcad_timeline_normalize(const char *timeline_json);

/* ── Fabrication: sheet metal, 3D print, multi-tool CAM ──────────────────
 * All return owned JSON (free with vcad_string_free) or null +
 * vcad_last_error. */
/* Chain API {"op":"evaluate"|"check"|"cost"|"nest"|"nested_dxf"|"sequence"|
 * "folded_step"|"materials"|"bend_table"|"shop_catalog", "chain"?, ...}. */
char *vcad_sheet_metal_call(const char *request_json);
/* Flat pattern from a solid. request (nullable) {"part"?,"material"?,
 * "shopProfile"?,"kernelVolumeTol"?} -> {flat_pattern, model, dxf, bends,
 * kernelVolumeCheck:{pass,...}, error?}. Fails closed (empty dxf). */
char *vcad_sheet_metal_flatten(const char *doc_json, const char *request_json);
/* {"op":"profiles"|"analyze"|"slice"|"export_3mf", "settings"?, ...};
 * doc_json may be null for "profiles". slice writes "gcodePath" /
 * "threeMfPath" when given. */
char *vcad_print_call(const char *doc_json, const char *request_json);
/* Multi-tool job {"stock","tools","operations","post"?,"target"?,...} ->
 * {gcode, programs[], toolChanges, moves[], operations[], verification}. */
char *vcad_cam_job(const char *request_json);
/* {"tools":[...]} — the kernel's default tool library. */
char *vcad_cam_tool_library(void);
/* --- Tuned instruments (instrument.rs) ---
 * doc = IR Document with `instrument` metadata (design_instrument).
 * {"op":"receipt"} | {"op":"record","measured_hz",note?,instrument?,measured_at?}
 * (-> {bar, note, claim, receipt, document}) | {"op":"cam_jobs","note",post?}
 * | {"op":"mill","note","out_dir",post?} | {"op":"print","plate":"bars"|"frame",
 * "gcode_path",settings?}. mill/print are DRY RUNS: files only. */
char *vcad_instrument_call(const char *doc_json, const char *request_json);
/* --- How parts sound (sound.rs) ---
 * {"op":"modes","part",modes?,quality?:"preview"|"fine",material?} -> modes
 *   (hz, note, cents, shape[]) + animation mesh (positions, indices)
 * | {"op":"lock","part",mode?,"target_hz",note?,"drives",tolerance_cents?}
 *   -> {document, report} | {"op":"unlock","part",mode?} -> {document}
 * | {"op":"check"} -> {locks:[{lock,hz,cents,holds}]}. */
char *vcad_sound_call(const char *doc_json, const char *request_json);
/* --- Electronics co-design (pcb.rs, schematic.rs) ---
 * Board = IR Pcb JSON (camelCase, as stored under doc.pcb / a PcbBoard node);
 * sheet = IR SchematicSheet JSON. All return owned JSON freed ONLY with
 * vcad_string_free; null + vcad_last_error on failure (fail-closed).
 * DRC reports carry {total, baseline_total, route_attributable, route_fixable,
 * unrouted, clean, findings[], rules[]}; "route-attributable" is measured
 * against the same board with all routing stripped. */
/* .kicad_pcb | .brd -> {pcb, warnings[], footprints, nets, traces, vias} */
char *vcad_pcb_import(const char *path);
/* request {"accept"?:[rule]} -> DRC report */
char *vcad_pcb_drc(const char *pcb_json, const char *request_json);
/* route unrouted connections -> {pcb, verdict, drc} */
char *vcad_pcb_autoroute(const char *pcb_json, const char *request_json);

/** Board held open for interactive routing. */
typedef struct VcadPcbRouter VcadPcbRouter;
/** Airwires {wires:[{net,from,to}]}: the DRC's own connectivity. */
char *vcad_pcb_ratsnest(const char *pcb_json);
char *vcad_pcb_fill_zones(const char *pcb_json);
char *vcad_pcb_library_footprint(const char *request_json);
char *vcad_pcb_route_diff_pair(const char *pcb_json, const char *request_json);
char *vcad_pcb_library_symbol(const char *request_json);
char *vcad_pcb_stitch_zones(const char *pcb_json, const char *request_json);
VcadPcbRouter *vcad_pcb_router_open(const char *pcb_json);
void vcad_pcb_router_free(VcadPcbRouter *router);
/** Route {net,from,to,layer?,budget?} -> {status,reason,path,vias,width}. */
char *vcad_pcb_router_route(const VcadPcbRouter *router, const char *request_json);
/** Commit {net,path} -> {traces,vias} to append to the board. */
char *vcad_pcb_router_commit(const VcadPcbRouter *router, const char *request_json);
/* fab-prep fix loop; request {"calibrate"?,"route"?,"max_rounds"?,"accept"?}
 * -> {pcb, converged, headline, summary, report, drc} */
char *vcad_pcb_fix_drc(const char *pcb_json, const char *request_json);
/* request {"dir", "accept"?} -> {dir, files[], drc}. REFUSES (null) unless
 * route_attributable == 0; nothing is written on refusal. */
char *vcad_pcb_export_gerbers(const char *pcb_json, const char *request_json);
/* request {"nets":[..],"target"?,"tolerance"?,"max_amplitude"?} -> {pcb, result, drc};
 * without "nets" -> {"lengths":[[net, mm]]} */
char *vcad_pcb_length_match(const char *pcb_json, const char *request_json);
/* request {"part","clearance"?,"placement"?} -> {report: EnclosureFitReport, features} */
char *vcad_pcb_enclosure_fit(const char *doc_json, const char *pcb_json, const char *request_json);
/* request {"part","clearance"?,"thickness"?,"hole_diameter"?} -> {pcb, placement, derived} */
char *vcad_pcb_board_from_solid(const char *doc_json, const char *request_json);
/* request {"name"?,"x"?,"y"?,"z"?} -> updated document JSON (board as a solid part) */
char *vcad_pcb_solid_from_board(const char *doc_json, const char *pcb_json, const char *request_json);
/* request {"clearance"?,"wall"?,"floor"?,"depth"?,"name"?} -> {document, placement, part} */
char *vcad_pcb_enclosure_for_board(const char *doc_json, const char *pcb_json, const char *request_json);
/* request {"ground"?,"tstop"?,"dt"?,"probes"?} -> {dc:{net:V}, currents:{ref:A}, transient?, stubbed[]} */
char *vcad_schematic_simulate(const char *schematic_json, const char *request_json);
/* -> {count, violations[]} */
char *vcad_schematic_erc(const char *schematic_json);

/* --- MCP core hosted by the app (mcp.rs) ---
 * vcad.app runs the Rust MCP core (crates/vcad-mcp): one dispatcher, the same
 * tools `vcad mcp` serves headless. The core calls `host` (on its worker
 * threads, never the caller's) with a JSON request {op, ...} and frees the
 * malloc'ed (strdup) JSON reply. Ops: front, open, read, apply, release,
 * documents, undo, open_path, live_resources, read_resource, engine_tool,
 * host_call, instance_meta -- see crates/vcad-ffi/src/mcp.rs. */
typedef struct VcadMcpApp VcadMcpApp;
typedef char *(*vcad_mcp_host_fn)(void *ctx, const char *request_json);
/* config {name?, version?, instructions?, port?, attempts?, serve_http?,
 * proxy?, node_cmd?, viewer_uri?}; null + vcad_last_error when no port binds. */
VcadMcpApp *vcad_mcp_app_start(vcad_mcp_host_fn host, void *ctx, const char *config_json);
uint16_t vcad_mcp_app_port(const VcadMcpApp *app);
/* One JSON-RPC message through the dispatcher; owned reply (vcad_string_free)
 * or null for a notification. Never call on the thread `host` needs. */
char *vcad_mcp_app_call(const VcadMcpApp *app, const char *msg_json);
void vcad_mcp_app_resource_updated(const VcadMcpApp *app, const char *uri);
/* id null = the front window changed. */
void vcad_mcp_app_document_changed(const VcadMcpApp *app, const char *id);
/* host_tools.json, owned (vcad_string_free). */
char *vcad_mcp_host_tools(void);

#ifdef __cplusplus
}
#endif

#endif /* VCAD_FFI_H */
