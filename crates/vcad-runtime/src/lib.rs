//! Per-document runtime: one thread owns the CRDT document
//! ([`vcad_app::DocumentApi`]) and the incremental evaluation cache, so a
//! host's UI thread never materializes, serializes or tessellates.
//!
//! Hosts talk to it two ways (see `docs/runtime-design.md`):
//!
//! - [`Runtime::send`] — a fire-and-forget [`Intent`] (parameter scrub,
//!   gesture command). Queued intents are coalesced (a scrub keeps only its
//!   latest value) and the result comes back as an immutable [`Frame`]
//!   through the sink: the materialized state, its IR bytes, and the
//!   evaluated scene, all built on the runtime thread.
//! - [`Runtime::call`] — a blocking closure run on the runtime thread, in
//!   inbox order with the intents. The C ABI's existing `vcad_engine_*`
//!   entry points go through this, so every client (UI, agent, MCP host
//!   hooks) is serialized at one point.
//!
//! Every state change bumps [`Core::rev`]; frames carry it so a host can
//! drop a frame that a later blocking edit already superseded.

#![warn(missing_docs)]

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde::Deserialize;
use serde_json::Value;
use vcad_app::command::EngineEnvelope;
use vcad_app::DocumentApi;
use vcad_crdt::ReplicaId;
use vcad_eval::evaluate::{evaluate_document_incremental, DisplayChordGuard};
use vcad_eval::incremental::IncrementalCache;
use vcad_eval::{EvalOptions, EvaluatedScene};
use vcad_ir::Document;

/// Where a gesture-scoped intent sits in its gesture. The first intent of a
/// gesture (`start`) snapshots the document so the whole gesture undoes as
/// one step ([`Core::swap_gesture`]).
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub struct Gesture {
    /// Host-chosen id, unique per gesture.
    pub id: u64,
    /// First event of the gesture.
    #[serde(default)]
    pub start: bool,
}

/// A fire-and-forget request. JSON: `{"type":"set_doc_param",…}`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Intent {
    /// One `DocCommand` (JSON object).
    Command {
        /// The command, as the `vcad_engine_apply` vocabulary.
        command: Value,
        /// Gesture this command belongs to, if any.
        #[serde(default)]
        gesture: Option<Gesture>,
        /// Consecutive queued commands with the same key in the same
        /// gesture collapse to the last one (e.g. `"node:12:offset:x"`).
        #[serde(default)]
        coalesce: Option<String>,
    },
    /// Overwrite a document-level literal parameter's value (formula
    /// parameters and unknown names are left alone), reloading the
    /// document — the same edit as the host's IR-edit path.
    SetDocParam {
        /// Parameter name.
        name: String,
        /// New value.
        value: f64,
        /// Gesture this scrub belongs to, if any.
        #[serde(default)]
        gesture: Option<Gesture>,
    },
    /// The gesture finished. Frames produced for it say so, so the host can
    /// do end-of-gesture work (collision shapes, re-centering).
    GestureEnd {
        /// The gesture's id.
        gesture: u64,
    },
}

impl Intent {
    fn gesture(&self) -> Option<Gesture> {
        match self {
            Intent::Command { gesture, .. } | Intent::SetDocParam { gesture, .. } => *gesture,
            Intent::GestureEnd { .. } => None,
        }
    }

    /// Fold `next` into `self` when it supersedes it: same gesture, same
    /// target. Keeps `start` if either was the start.
    fn absorb(&mut self, next: &Intent) -> bool {
        let start = |a: Option<Gesture>, b: Option<Gesture>| match (a, b) {
            (Some(a), Some(b)) if a.id == b.id => Some(Gesture {
                id: a.id,
                start: a.start || b.start,
            }),
            _ => None,
        };
        match (&mut *self, next) {
            (
                Intent::SetDocParam {
                    name,
                    value,
                    gesture,
                },
                Intent::SetDocParam {
                    name: n2,
                    value: v2,
                    gesture: g2,
                },
            ) if name == n2 => {
                let Some(g) = start(*gesture, *g2) else {
                    return false;
                };
                *value = *v2;
                *gesture = Some(g);
                true
            }
            (
                Intent::Command {
                    command,
                    gesture,
                    coalesce: Some(k),
                },
                Intent::Command {
                    command: c2,
                    gesture: g2,
                    coalesce: Some(k2),
                },
            ) if k == k2 => {
                let Some(g) = start(*gesture, *g2) else {
                    return false;
                };
                *command = c2.clone();
                *gesture = Some(g);
                true
            }
            _ => false,
        }
    }
}

/// An intent with the host's sequence number (`{"seq":7,"intent":{…}}`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Envelope {
    /// Monotonic per host; frames echo the last one folded in.
    pub seq: u64,
    /// The request.
    pub intent: Intent,
}

/// What the runtime publishes after applying a batch of intents. Immutable;
/// everything in it was built on the runtime thread.
pub struct Frame {
    /// Document revision this frame describes.
    pub rev: u64,
    /// Last intent `seq` folded into `rev`.
    pub acked_seq: u64,
    /// The state as an `ok` result envelope (`vcad_engine_state` shape).
    pub state: String,
    /// The materialized IR document as JSON bytes.
    pub document_json: Vec<u8>,
    /// The materialized IR document.
    pub document: Document,
    /// The evaluated scene (visible roots, in order). `None` when
    /// evaluation failed (see `eval_error`) or is disabled.
    pub scene: Option<EvaluatedScene>,
    /// Why evaluation produced no scene.
    pub eval_error: Option<String>,
    /// Refusals from intents in this batch (the document excludes them).
    pub errors: Vec<String>,
    /// Gestures that ended in this batch.
    pub ended_gestures: Vec<u64>,
    /// Whether a gesture is still open after this batch.
    pub gesture_open: bool,
    /// [`DocumentApi::state_hash`] of the state (the host's dirty check).
    pub state_hash: String,
    /// [`DocumentApi::feature_history`] as JSON (the history tree's rows),
    /// so the host never makes a blocking read to redraw it.
    pub history_json: String,
}

/// Receives frames, on the runtime thread.
pub type Sink = Box<dyn Fn(Frame) + Send + 'static>;

/// Runtime settings.
#[derive(Debug, Clone)]
pub struct Config {
    /// CRDT replica id.
    pub replica: u64,
    /// Display chord tolerance (mm) for frame tessellation; ≤ 0 disables.
    pub display_chord_mm: f64,
    /// Thread stack size (kernel recursion is deep).
    pub stack_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            replica: 1,
            display_chord_mm: 0.02,
            stack_size: 64 << 20,
        }
    }
}

/// The state the runtime thread owns. Blocking [`Runtime::call`] closures
/// get `&mut Core`.
pub struct Core {
    /// The document.
    pub api: DocumentApi,
    cache: IncrementalCache,
    gestures: HashMap<u64, Vec<u8>>,
    open_gestures: Vec<u64>,
    rev: u64,
    /// Directory relative mesh imports resolve against.
    pub base_dir: Option<PathBuf>,
    display_chord_mm: f64,
}

impl Core {
    fn new(cfg: &Config) -> Self {
        Self {
            api: DocumentApi::new(ReplicaId(cfg.replica)),
            cache: IncrementalCache::new(),
            gestures: HashMap::new(),
            open_gestures: Vec::new(),
            rev: 0,
            base_dir: None,
            display_chord_mm: cfg.display_chord_mm,
        }
    }

    /// Current revision.
    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Record a state change (blocking edits call this so in-flight frames
    /// from before it are recognizably stale).
    pub fn bump(&mut self) -> u64 {
        self.rev += 1;
        self.rev
    }

    /// Replace the document wholesale (open / IR reload). Drops gesture
    /// snapshots' validity only in that they still restore what they held.
    pub fn replace(&mut self, api: DocumentApi) {
        self.api = api;
        self.bump();
    }

    /// Undo/redo a gesture: restore the snapshot taken at its start and keep
    /// the current state in its place, so calling it again redoes.
    pub fn swap_gesture(&mut self, id: u64) -> Result<(), String> {
        let snap = self
            .gestures
            .get(&id)
            .ok_or_else(|| format!("no gesture {id}"))?
            .clone();
        let current = self.api.save();
        self.api.restore_snapshot(&snap)?;
        self.gestures.insert(id, current);
        self.bump();
        Ok(())
    }

    /// Forget a gesture snapshot (its undo entry fell off the host's stack).
    pub fn forget_gesture(&mut self, id: u64) {
        self.gestures.remove(&id);
    }

    fn apply(&mut self, intent: Intent) -> Result<(), String> {
        if let Some(g) = intent.gesture() {
            if g.start {
                self.gestures.insert(g.id, self.api.save());
            }
            if !self.open_gestures.contains(&g.id) {
                self.open_gestures.push(g.id);
            }
        }
        match intent {
            Intent::Command { command, .. } => self
                .api
                .apply_json(&command.to_string())
                .map(drop)
                .map_err(|e| e.to_string()),
            Intent::SetDocParam { name, value, .. } => {
                let mut doc = serde_json::to_value(self.api.get_state().document)
                    .map_err(|e| e.to_string())?;
                let slot = doc
                    .get_mut("parameters")
                    .and_then(|p| p.get_mut(&name))
                    .and_then(|p| p.get_mut("value"))
                    .filter(|v| v.is_number())
                    .ok_or_else(|| format!("no literal parameter `{name}`"))?;
                *slot = serde_json::json!(value);
                let replica = self.api.crdt().replica_id();
                self.api = DocumentApi::from_ir(&doc.to_string(), replica)?;
                Ok(())
            }
            Intent::GestureEnd { gesture } => {
                self.open_gestures.retain(|g| *g != gesture);
                Ok(())
            }
        }
    }

    fn frame(&mut self, acked_seq: u64, errors: Vec<String>, ended: Vec<u64>) -> Frame {
        let state = self.api.get_state();
        let document = state.document.clone();
        let document_json = serde_json::to_vec(&document).unwrap_or_default();
        let state = serde_json::to_string(&EngineEnvelope::from(Ok(state))).unwrap_or_default();
        let (scene, eval_error) = self.evaluate(&document);
        Frame {
            rev: self.rev,
            acked_seq,
            state,
            document_json,
            document,
            scene,
            eval_error,
            errors,
            ended_gestures: ended,
            gesture_open: !self.open_gestures.is_empty(),
            state_hash: self.api.state_hash(),
            history_json: serde_json::to_string(&self.api.feature_history()).unwrap_or_default(),
        }
    }

    fn evaluate(&mut self, document: &Document) -> (Option<EvaluatedScene>, Option<String>) {
        let mut doc = document.clone();
        if let Some(dir) = &self.base_dir {
            vcad_eval::resolve_mesh_paths(&mut doc, dir);
        }
        let chord = self.display_chord_mm;
        let cache = &mut self.cache;
        let out = catch_unwind(AssertUnwindSafe(|| {
            let _g = DisplayChordGuard::enable(chord);
            evaluate_document_incremental(&doc, &EvalOptions::default(), cache)
        }));
        match out {
            Ok(Ok(scene)) => (Some(scene), None),
            Ok(Err(e)) => (None, Some(format!("evaluate: {e}"))),
            Err(_) => {
                // A panic mid-evaluation can leave the cache half-updated.
                self.cache.clear();
                (None, Some("evaluation panicked".into()))
            }
        }
    }
}

type Job = Box<dyn FnOnce(&mut Core) + Send>;

enum Msg {
    Intent(Envelope),
    Call(Job),
}

/// A running document runtime. Dropping it stops the thread; no frame is
/// delivered after `drop` returns.
pub struct Runtime {
    tx: Option<Sender<Msg>>,
    thread: Option<JoinHandle<()>>,
    sink: Arc<Mutex<Option<Sink>>>,
}

impl Runtime {
    /// Start a runtime with an empty document.
    pub fn new(cfg: Config) -> Self {
        let (tx, rx) = mpsc::channel();
        let sink: Arc<Mutex<Option<Sink>>> = Arc::new(Mutex::new(None));
        let thread_sink = sink.clone();
        let thread = std::thread::Builder::new()
            .name("vcad-doc-runtime".into())
            .stack_size(cfg.stack_size)
            .spawn(move || run(Core::new(&cfg), rx, thread_sink))
            .expect("spawn vcad-doc-runtime");
        Self {
            tx: Some(tx),
            thread: Some(thread),
            sink,
        }
    }

    /// Install (or clear) the frame sink.
    pub fn set_sink(&self, sink: Option<Sink>) {
        *self.sink.lock().unwrap_or_else(|e| e.into_inner()) = sink;
    }

    /// Queue an intent. Never blocks. False if the runtime has stopped.
    pub fn send(&self, envelope: Envelope) -> bool {
        self.tx
            .as_ref()
            .is_some_and(|tx| tx.send(Msg::Intent(envelope)).is_ok())
    }

    /// Run `f` on the runtime thread, after everything already queued, and
    /// wait for its result. `Err` if the runtime stopped or `f` panicked.
    pub fn call<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Core) -> R + Send + 'static,
    ) -> Result<R, String> {
        let (rtx, rrx) = mpsc::sync_channel(1);
        let job: Job = Box::new(move |core| {
            let _ = rtx.send(f(core));
        });
        let tx = self.tx.as_ref().ok_or("runtime stopped")?;
        tx.send(Msg::Call(job))
            .map_err(|_| "runtime stopped".to_string())?;
        rrx.recv().map_err(|_| "runtime call panicked".to_string())
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.set_sink(None);
        drop(self.tx.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Apply queued intents in order, coalesced; blocking calls are barriers.
fn run(mut core: Core, rx: Receiver<Msg>, sink: Arc<Mutex<Option<Sink>>>) {
    let mut queue: std::collections::VecDeque<Msg> = Default::default();
    loop {
        if queue.is_empty() {
            match rx.recv() {
                Ok(m) => queue.push_back(m),
                Err(_) => return,
            }
        }
        queue.extend(rx.try_iter());
        // Take the leading run of intents (up to the next call), coalesced.
        let mut run_: Vec<Envelope> = Vec::new();
        while let Some(Msg::Intent(_)) = queue.front() {
            let Some(Msg::Intent(env)) = queue.pop_front() else {
                unreachable!()
            };
            let absorbed = run_.last_mut().is_some_and(|last| {
                let hit = last.intent.absorb(&env.intent);
                if hit {
                    last.seq = env.seq;
                }
                hit
            });
            if !absorbed {
                run_.push(env);
            }
        }
        if !run_.is_empty() {
            let acked = run_.last().map_or(0, |e| e.seq);
            let mut errors = Vec::new();
            let mut ended = Vec::new();
            for env in run_ {
                if let Intent::GestureEnd { gesture } = env.intent {
                    ended.push(gesture);
                }
                let r = catch_unwind(AssertUnwindSafe(|| core.apply(env.intent)))
                    .unwrap_or_else(|_| Err("intent panicked".into()));
                if let Err(e) = r {
                    errors.push(e);
                }
            }
            core.bump();
            // Latest wins: if more intents arrived meanwhile, fold them in
            // before paying for a materialize + evaluation.
            queue.extend(rx.try_iter());
            if matches!(queue.front(), Some(Msg::Intent(_)))
                && ended.is_empty()
                && errors.is_empty()
            {
                continue;
            }
            let frame = core.frame(acked, errors, ended);
            if let Some(s) = sink.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                s(frame);
            }
            continue;
        }
        if let Some(Msg::Call(job)) = queue.pop_front() {
            // A panicking job drops its reply sender; the caller sees Err.
            let _ = catch_unwind(AssertUnwindSafe(|| job(&mut core)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;
    use std::time::Duration;

    const DOC: &str = r#"{"version":"0.1","nodes":{
        "1":{"id":1,"op":{"type":"Cube","size":{"x":10,"y":10,"z":10}}}},
        "materials":{},"part_materials":{},
        "parameters":{"w":{"value":10.0}},
        "roots":[{"root":1,"material":"a"}]}"#;

    fn runtime() -> (Runtime, Receiver<Frame>) {
        let rt = Runtime::new(Config::default());
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        rt.set_sink(Some(Box::new(move |f| {
            let _ = tx.lock().unwrap().send(f);
        })));
        rt.call(|c| {
            let api = DocumentApi::from_ir(DOC, ReplicaId(1)).unwrap();
            c.replace(api);
        })
        .unwrap();
        (rt, rx)
    }

    fn param(doc: &[u8]) -> f64 {
        let v: Value = serde_json::from_slice(doc).unwrap();
        v["parameters"]["w"]["value"].as_f64().unwrap()
    }

    fn scrub(seq: u64, value: f64, start: bool) -> Envelope {
        Envelope {
            seq,
            intent: Intent::SetDocParam {
                name: "w".into(),
                value,
                gesture: Some(Gesture { id: 9, start }),
            },
        }
    }

    #[test]
    fn intent_json_round_trip() {
        let e: Envelope = serde_json::from_str(
            r#"{"seq":3,"intent":{"type":"set_doc_param","name":"w","value":2,"gesture":{"id":1,"start":true}}}"#,
        )
        .unwrap();
        assert_eq!(
            e,
            Envelope {
                seq: 3,
                intent: Intent::SetDocParam {
                    name: "w".into(),
                    value: 2.0,
                    gesture: Some(Gesture { id: 1, start: true })
                }
            }
        );
        let c: Envelope = serde_json::from_str(
            r#"{"seq":4,"intent":{"type":"command","command":{"type":"undo"},"coalesce":"k"}}"#,
        )
        .unwrap();
        assert!(matches!(c.intent, Intent::Command { .. }));
    }

    #[test]
    fn frame_carries_state_bytes_and_scene() {
        let (rt, rx) = runtime();
        assert!(rt.send(scrub(1, 12.0, true)));
        let f = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(f.acked_seq, 1);
        assert_eq!(param(&f.document_json), 12.0);
        assert!(f.state.starts_with(r#"{"ok":true"#));
        assert_eq!(f.scene.as_ref().map(|s| s.parts.len()), Some(1));
        assert!(f.gesture_open);
        assert!(f.errors.is_empty());
    }

    #[test]
    fn queued_scrubs_coalesce_to_the_latest() {
        let (rt, rx) = runtime();
        // Hold the runtime busy so the scrubs queue up behind it.
        let (go_tx, go_rx) = sync_channel::<()>(0);
        let (busy_tx, busy_rx) = sync_channel::<()>(0);
        std::thread::scope(|s| {
            s.spawn(|| {
                rt.call(move |_| {
                    busy_tx.send(()).unwrap();
                    go_rx.recv().unwrap();
                })
                .unwrap();
            });
            busy_rx.recv().unwrap();
            for i in 0..20 {
                rt.send(scrub(i + 1, 10.0 + i as f64, i == 0));
            }
            rt.send(Envelope {
                seq: 21,
                intent: Intent::GestureEnd { gesture: 9 },
            });
            go_tx.send(()).unwrap();
        });
        let f = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(f.acked_seq, 21);
        assert_eq!(param(&f.document_json), 29.0);
        assert_eq!(f.ended_gestures, vec![9]);
        assert!(!f.gesture_open);
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "one frame for the batch"
        );
        // The whole gesture undoes as one step, and swapping again redoes.
        let undone = rt.call(|c| {
            c.swap_gesture(9).unwrap();
            serde_json::to_vec(&c.api.get_state().document).unwrap()
        });
        assert_eq!(param(&undone.unwrap()), 10.0);
        let redone = rt.call(|c| {
            c.swap_gesture(9).unwrap();
            serde_json::to_vec(&c.api.get_state().document).unwrap()
        });
        assert_eq!(param(&redone.unwrap()), 29.0);
    }

    #[test]
    fn calls_are_ordered_after_queued_intents_and_bump_rev() {
        let (rt, rx) = runtime();
        rt.send(scrub(1, 15.0, true));
        let (rev, w) = rt
            .call(|c| {
                (
                    c.rev(),
                    param(&serde_json::to_vec(&c.api.get_state().document).unwrap()),
                )
            })
            .unwrap();
        assert_eq!(w, 15.0, "the call saw the scrub queued before it");
        let f = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(f.rev <= rev);
    }

    #[test]
    fn refused_intent_reports_and_keeps_document() {
        let (rt, rx) = runtime();
        rt.send(Envelope {
            seq: 1,
            intent: Intent::SetDocParam {
                name: "nope".into(),
                value: 1.0,
                gesture: None,
            },
        });
        let f = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(f.errors.len(), 1);
        assert_eq!(param(&f.document_json), 10.0);
    }

    #[test]
    fn panicking_call_is_an_error_not_a_dead_runtime() {
        let (rt, _rx) = runtime();
        assert!(rt.call(|_| -> u8 { panic!("boom") }).is_err());
        assert_eq!(rt.call(|_| 7).unwrap(), 7);
    }
}
