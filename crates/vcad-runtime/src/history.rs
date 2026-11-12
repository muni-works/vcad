//! The document's undo history, owned by the runtime so every client (the
//! app, its agent, MCP, the CLI) shares one history (see
//! `docs/runtime-design.md`, "Undo in Rust").
//!
//! Two kinds of entry, matching the host's old Swift stack exactly:
//! - **Engine**: exactly one engine history group — undone and redone
//!   through the CRDT's own `undo`/`redo`.
//! - **Snapshot**: the whole engine state (document + its CRDT history) from
//!   before a compound edit, a gesture or an IR reload — undone by swapping
//!   it with the current state, so the same entry redoes.

use std::collections::HashMap;

use serde::Serialize;
use vcad_app::DocumentApi;

/// Entries kept; the oldest falls off past this.
pub const CAPACITY: usize = 64;

#[derive(Clone)]
enum Step {
    Engine,
    Snapshot(Vec<u8>),
}

#[derive(Clone)]
struct Entry {
    label: String,
    step: Step,
}

/// Undo and redo stacks plus pending snapshot marks.
#[derive(Default)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    marks: HashMap<u64, Vec<u8>>,
    next_mark: u64,
}

/// The labels of both stacks, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Labels {
    /// Undo stack, bottom to top (the last one is what ⌘Z undoes).
    pub undo: Vec<String>,
    /// Redo stack, bottom to top (the last one is what ⇧⌘Z redoes).
    pub redo: Vec<String>,
}

impl History {
    /// Both stacks' labels.
    pub fn labels(&self) -> Labels {
        Labels {
            undo: self.undo.iter().map(|e| e.label.clone()).collect(),
            redo: self.redo.iter().map(|e| e.label.clone()).collect(),
        }
    }

    fn push(&mut self, label: &str, step: Step) {
        self.undo.push(Entry {
            label: label.to_string(),
            step,
        });
        if self.undo.len() > CAPACITY {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// One engine history group was just applied.
    pub fn push_engine(&mut self, label: &str) {
        self.push(label, Step::Engine);
    }

    /// A snapshot the caller holds (bytes of [`DocumentApi::save`]).
    pub fn push_snapshot(&mut self, label: &str, snapshot: Vec<u8>) {
        self.push(label, Step::Snapshot(snapshot));
    }

    /// Remember the current state under a new mark (for an edit that lands
    /// as one entry only if it succeeds).
    pub fn mark(&mut self, api: &DocumentApi) -> u64 {
        self.next_mark += 1;
        self.marks.insert(self.next_mark, api.save());
        self.next_mark
    }

    /// Push the mark's snapshot as an entry.
    pub fn push_mark(&mut self, mark: u64, label: &str) -> Result<(), String> {
        let snap = self
            .marks
            .remove(&mark)
            .ok_or_else(|| format!("no mark {mark}"))?;
        self.push_snapshot(label, snap);
        Ok(())
    }

    /// Put the document back to the mark (a failed edit) and drop it.
    pub fn restore_mark(&mut self, mark: u64, api: &mut DocumentApi) -> Result<(), String> {
        let snap = self
            .marks
            .remove(&mark)
            .ok_or_else(|| format!("no mark {mark}"))?;
        api.restore_snapshot(&snap)
    }

    /// Forget a mark.
    pub fn drop_mark(&mut self, mark: u64) {
        self.marks.remove(&mark);
    }

    /// Undo the top entry; returns its label (`None`: nothing to undo).
    pub fn undo(&mut self, api: &mut DocumentApi) -> Result<Option<String>, String> {
        let Some(mut e) = self.undo.pop() else {
            return Ok(None);
        };
        if let Err(err) = Self::step(&mut e, api, false) {
            self.undo.push(e);
            return Err(err);
        }
        let label = e.label.clone();
        self.redo.push(e);
        Ok(Some(label))
    }

    /// Redo the top redo entry; returns its label.
    pub fn redo(&mut self, api: &mut DocumentApi) -> Result<Option<String>, String> {
        let Some(mut e) = self.redo.pop() else {
            return Ok(None);
        };
        if let Err(err) = Self::step(&mut e, api, true) {
            self.redo.push(e);
            return Err(err);
        }
        let label = e.label.clone();
        self.undo.push(e);
        Ok(Some(label))
    }

    /// Apply `e` in `forward` (redo) or backward (undo) direction, leaving
    /// in it what reverses it.
    fn step(e: &mut Entry, api: &mut DocumentApi, forward: bool) -> Result<(), String> {
        match &mut e.step {
            Step::Engine => {
                let cmd = if forward {
                    vcad_app::DocCommand::Redo
                } else {
                    vcad_app::DocCommand::Undo
                };
                api.apply(cmd).map(drop).map_err(|e| e.to_string())
            }
            Step::Snapshot(snap) => {
                let current = api.save();
                api.restore_snapshot(snap)?;
                *snap = current;
                Ok(())
            }
        }
    }

    /// Drop undo entries above `depth` (an agent edit folding what it pushed
    /// into one entry).
    pub fn truncate(&mut self, depth: usize) {
        self.undo.truncate(depth);
    }

    /// Rename the top undo entry; false when there is none.
    pub fn rename_top(&mut self, label: &str) -> bool {
        match self.undo.last_mut() {
            Some(e) => {
                e.label = label.to_string();
                true
            }
            None => false,
        }
    }

    /// Whether the top undo entry is a snapshot (one an agent turn can keep
    /// extending).
    pub fn top_is_snapshot(&self) -> bool {
        matches!(
            self.undo.last(),
            Some(Entry {
                step: Step::Snapshot(_),
                ..
            })
        )
    }

    /// Drop the redo stack (an edit after undoing, a step taken back).
    pub fn clear_redo(&mut self) {
        self.redo.clear();
    }

    /// Drop everything (a document replaced wholesale).
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.marks.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vcad_crdt::ReplicaId;

    const DOC: &str = r#"{"version":"0.1","nodes":{
        "1":{"id":1,"op":{"type":"Cube","size":{"x":10,"y":10,"z":10}}}},
        "materials":{},"part_materials":{},
        "roots":[{"root":1,"material":"a"}]}"#;

    fn material(api: &DocumentApi) -> String {
        let doc = serde_json::to_value(api.get_state().document).unwrap();
        doc["roots"][0]["material"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    fn part(api: &DocumentApi) -> String {
        api.get_state().parts[0].id().to_string()
    }

    #[test]
    fn engine_and_snapshot_entries_undo_and_redo_in_order() {
        let mut api = DocumentApi::from_ir(DOC, ReplicaId(1)).unwrap();
        let mut h = History::default();
        let id = part(&api);
        // An engine step.
        api.set_material(&id, "steel");
        h.push_engine("Steel");
        // A snapshot step (compound edit).
        let m = h.mark(&api);
        api.set_material(&id, "brass");
        api.set_material(&id, "copper");
        h.push_mark(m, "Copper").unwrap();
        assert_eq!(h.labels().undo, vec!["Steel", "Copper"]);

        assert_eq!(h.undo(&mut api).unwrap().as_deref(), Some("Copper"));
        assert_eq!(material(&api), "steel");
        assert_eq!(h.undo(&mut api).unwrap().as_deref(), Some("Steel"));
        assert_eq!(material(&api), "a");
        assert_eq!(h.labels().redo, vec!["Copper", "Steel"]);

        assert_eq!(h.redo(&mut api).unwrap().as_deref(), Some("Steel"));
        assert_eq!(material(&api), "steel");
        assert_eq!(h.redo(&mut api).unwrap().as_deref(), Some("Copper"));
        assert_eq!(material(&api), "copper");
        assert_eq!(h.undo(&mut api).unwrap(), Some("Copper".into()));
        // A new edit drops the redo stack.
        h.push_engine("Other");
        assert!(h.labels().redo.is_empty());
    }

    #[test]
    fn marks_restore_on_failure_and_capacity_caps() {
        let mut api = DocumentApi::from_ir(DOC, ReplicaId(1)).unwrap();
        let mut h = History::default();
        let id = part(&api);
        let m = h.mark(&api);
        api.set_material(&id, "steel");
        h.restore_mark(m, &mut api).unwrap();
        assert_eq!(material(&api), "a");
        assert!(h.labels().undo.is_empty());
        for i in 0..CAPACITY + 5 {
            h.push_engine(&format!("e{i}"));
        }
        let l = h.labels();
        assert_eq!(l.undo.len(), CAPACITY);
        assert_eq!(l.undo[0], "e5");
        h.truncate(2);
        assert!(h.rename_top("x"));
        assert_eq!(h.labels().undo, vec!["e5", "x"]);
        assert!(!h.top_is_snapshot());
    }
}
