//! Agent jobs: every long-running tool call (fab dry runs, optimize, FEA,
//! topology optimization, meshing, slicing…) is a *job* while it runs —
//! whichever era the client speaks and whether or not it asked for an MCP
//! Task. The job feed (`BusEvent::Job`) is what vcad.app draws in its header
//! progress pill; `vcad/jobs/cancel` is the app's ✕, and cancelling the MCP
//! Task (`tasks/cancel`) ends the job the same way.
//!
//! Wire shape of one job event:
//! `{"jobId", "tool", "state": "running"|"completed"|"failed"|"cancelled",
//!   "stage"?, "fraction"?, "taskId"?}`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::tasks::{Bus, BusEvent};
use crate::tools::ProgressFn;

/// Method: cancel a running job (`{"jobId"}`).
pub const METHOD_CANCEL: &str = "vcad/jobs/cancel";
/// Method: a host-executed job reports progress (`{"jobId", "fraction"?, "stage"?}`).
pub const METHOD_PROGRESS: &str = "vcad/jobs/progress";
/// Method: running jobs (`{"jobs": [...]}`).
pub const METHOD_LIST: &str = "vcad/jobs/list";

/// True for the job methods.
pub fn is_job_method(m: &str) -> bool {
    matches!(m, METHOD_CANCEL | METHOD_PROGRESS | METHOD_LIST)
}

struct Job {
    tool: String,
    task: bool,
    stage: Option<String>,
    fraction: Option<f64>,
    cancel: Arc<AtomicBool>,
    /// The call's own progress sink (MCP progress / Task status), fed by
    /// progress a host-executed job reports back.
    sink: Option<ProgressFn>,
}

impl Job {
    fn wire(&self, id: &str, state: &str) -> Value {
        let mut v = json!({"jobId": id, "tool": self.tool, "state": state});
        if let Some(s) = &self.stage {
            v["stage"] = json!(s);
        }
        if let Some(f) = self.fraction {
            v["fraction"] = json!(f);
        }
        if self.task {
            v["taskId"] = json!(id);
        }
        v
    }
}

/// Running jobs.
pub struct Jobs {
    next: AtomicU64,
    map: Mutex<HashMap<String, Job>>,
    bus: Arc<Bus>,
}

impl Jobs {
    /// New registry emitting on `bus`.
    pub fn new(bus: Arc<Bus>) -> Self {
        Jobs {
            next: AtomicU64::new(1),
            map: Mutex::new(HashMap::new()),
            bus,
        }
    }

    /// Start a job. `task_id` makes the job id the MCP Task id.
    pub fn start(&self, tool: &str, task_id: Option<&str>, cancel: Arc<AtomicBool>) -> String {
        self.start_with_sink(tool, task_id, cancel, None)
    }

    /// [`Jobs::start`], with the call's progress sink for [`Jobs::host_progress`].
    pub fn start_with_sink(
        &self,
        tool: &str,
        task_id: Option<&str>,
        cancel: Arc<AtomicBool>,
        sink: Option<ProgressFn>,
    ) -> String {
        let id = task_id.map(str::to_string).unwrap_or_else(|| {
            format!(
                "job_{}_{}",
                std::process::id(),
                self.next.fetch_add(1, Ordering::Relaxed)
            )
        });
        let job = Job {
            tool: tool.to_string(),
            task: task_id.is_some(),
            stage: None,
            fraction: None,
            cancel,
            sink,
        };
        let wire = job.wire(&id, "running");
        self.lock().insert(id.clone(), job);
        self.bus.emit(BusEvent::Job(wire));
        id
    }

    /// Progress: `fraction` in 0…1 (None = unmeasured), `stage` text.
    pub fn progress(&self, id: &str, fraction: Option<f64>, stage: Option<String>) {
        let wire = {
            let mut map = self.lock();
            let Some(j) = map.get_mut(id) else { return };
            if let Some(f) = fraction.filter(|f| f.is_finite()) {
                j.fraction = Some(f.clamp(0.0, 1.0));
            }
            if stage.is_some() {
                j.stage = stage;
            }
            j.wire(id, "running")
        };
        self.bus.emit(BusEvent::Job(wire));
    }

    /// Progress reported by the app for work it runs under the job: the job
    /// feed AND the call's own progress (the agent's Task status) move.
    pub fn host_progress(&self, id: &str, fraction: Option<f64>, stage: Option<String>) {
        let sink = self.lock().get(id).and_then(|j| j.sink.clone());
        match sink {
            // The sink is the wrapped progress: it updates the job too.
            Some(s) => s(
                fraction.unwrap_or(0.0) * 100.0,
                fraction.map(|_| 100.0),
                stage,
            ),
            None => self.progress(id, fraction, stage),
        }
    }

    /// The job ended. A set cancel flag wins over `state`.
    pub fn finish(&self, id: &str, state: &str) {
        let Some(j) = self.lock().remove(id) else {
            return;
        };
        let state = if j.cancel.load(Ordering::Relaxed) {
            "cancelled"
        } else {
            state
        };
        self.bus.emit(BusEvent::Job(j.wire(id, state)));
    }

    /// Set the job's cancel flag. Returns `Some(is_task)` when it was running.
    pub fn cancel(&self, id: &str) -> Option<bool> {
        let map = self.lock();
        let j = map.get(id)?;
        j.cancel.store(true, Ordering::Relaxed);
        Some(j.task)
    }

    /// Whether `id` was cancelled (false once ended or unknown).
    pub fn is_cancelled(&self, id: &str) -> bool {
        self.lock()
            .get(id)
            .is_some_and(|j| j.cancel.load(Ordering::Relaxed))
    }

    /// Running jobs, wire shape.
    pub fn list(&self) -> Value {
        let map = self.lock();
        let mut jobs: Vec<Value> = map.iter().map(|(id, j)| j.wire(id, "running")).collect();
        jobs.sort_by(|a, b| a["jobId"].as_str().cmp(&b["jobId"].as_str()));
        json!({ "jobs": jobs })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Job>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// `progress / total` as a fraction when it means one.
pub fn fraction(progress: f64, total: Option<f64>) -> Option<f64> {
    match total {
        Some(t) if t > 0.0 => Some(progress / t),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_and_cancel() {
        let bus = Arc::new(Bus::default());
        let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
        let s2 = seen.clone();
        bus.on(Arc::new(move |e| {
            if let BusEvent::Job(v) = e {
                s2.lock().unwrap().push(v.clone());
            }
        }));
        let jobs = Jobs::new(bus);
        let flag = Arc::new(AtomicBool::new(false));
        let id = jobs.start("fab_print", None, flag.clone());
        jobs.progress(&id, Some(0.5), Some("Slicing".into()));
        assert_eq!(jobs.list()["jobs"][0]["fraction"], 0.5);
        assert_eq!(jobs.cancel(&id), Some(false));
        assert!(flag.load(Ordering::Relaxed) && jobs.is_cancelled(&id));
        jobs.finish(&id, "completed");
        assert!(jobs.cancel(&id).is_none());
        let s = seen.lock().unwrap();
        let states: Vec<&str> = s.iter().map(|v| v["state"].as_str().unwrap()).collect();
        assert_eq!(states, ["running", "running", "cancelled"]);
        assert_eq!(s[1]["stage"], "Slicing");
    }

    #[test]
    fn task_jobs_share_the_task_id() {
        let jobs = Jobs::new(Arc::new(Bus::default()));
        let id = jobs.start("x", Some("task_ab"), Arc::default());
        assert_eq!(id, "task_ab");
        assert_eq!(jobs.list()["jobs"][0]["taskId"], "task_ab");
        assert_eq!(fraction(2.0, Some(4.0)), Some(0.5));
        assert_eq!(fraction(2.0, None), None);
    }
}
