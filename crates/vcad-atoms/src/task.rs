//! A minimal MCP-Tasks-shaped runtime: progress, cancellation, terminal state.
//!
//! The MCP Tasks extension (SEP-2663) models a long-running tool call as a
//! task with a status (`working` → `completed` | `failed` | `cancelled`),
//! optional progress notifications, and a `tasks/cancel` request. This module
//! is the server-side half of that contract with no transport attached:
//! `crates/vcad-mcp` (M0) wraps a [`TaskHandle`] and forwards
//! [`TaskSnapshot`]s as `notifications/progress` / `tasks/get` results.
//!
//! Workers are cooperative: a job calls [`TaskCtx::progress`] at stage
//! boundaries and [`TaskCtx::checkpoint`] in its inner loops, which returns
//! [`Cancelled`] once a cancel was requested. Nothing is ever killed mid-write.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use serde::Serialize;
use serde_json::Value;

/// Task status, matching the MCP Tasks status vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Still running.
    Working,
    /// Finished with a result.
    Completed,
    /// Finished with an error (including a failed verify gate).
    Failed,
    /// Stopped because a cancel was requested.
    Cancelled,
}

impl TaskStatus {
    /// Whether the task has reached a terminal state.
    pub fn is_terminal(self) -> bool {
        !matches!(self, TaskStatus::Working)
    }
}

/// One progress notification.
#[derive(Debug, Clone, Serialize)]
pub struct ProgressEvent {
    /// Fraction done in `[0, 1]`; never decreases within one task.
    pub progress: f64,
    /// Short human-readable stage message.
    pub message: String,
}

/// A point-in-time view of a task, the payload for `tasks/get`.
#[derive(Debug, Clone, Serialize)]
pub struct TaskSnapshot {
    /// Current status.
    pub status: TaskStatus,
    /// Latest progress fraction.
    pub progress: f64,
    /// Latest progress message.
    pub message: String,
    /// Result payload once `completed` (and, for failed verify gates, the
    /// gate report, so the agent can see *why* it failed closed).
    pub result: Option<Value>,
    /// Error text once `failed`.
    pub error: Option<String>,
}

/// Marker error: the job observed a cancel request and stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

/// Error type returned by task jobs.
#[derive(Debug, Clone)]
pub enum JobError {
    /// Cancel observed.
    Cancelled,
    /// The job failed. `report` carries structured detail (e.g. a gate
    /// report) when there is one.
    Failed {
        /// Human-readable reason.
        message: String,
        /// Optional structured detail.
        report: Option<Value>,
    },
}

impl From<Cancelled> for JobError {
    fn from(_: Cancelled) -> Self {
        JobError::Cancelled
    }
}

impl From<String> for JobError {
    fn from(message: String) -> Self {
        JobError::Failed {
            message,
            report: None,
        }
    }
}

impl From<&str> for JobError {
    fn from(message: &str) -> Self {
        JobError::from(message.to_string())
    }
}

#[derive(Debug)]
struct Inner {
    status: TaskStatus,
    events: Vec<ProgressEvent>,
    result: Option<Value>,
    error: Option<String>,
}

#[derive(Debug)]
struct Shared {
    cancel: AtomicBool,
    inner: Mutex<Inner>,
    done: Condvar,
}

/// The job's side of a task: report progress, observe cancellation.
#[derive(Debug, Clone)]
pub struct TaskCtx {
    shared: Arc<Shared>,
}

impl TaskCtx {
    /// A context not attached to any spawned task — for running a job
    /// synchronously (CLI, tests). Progress is still recorded.
    pub fn detached() -> Self {
        TaskCtx {
            shared: Arc::new(Shared {
                cancel: AtomicBool::new(false),
                inner: Mutex::new(Inner {
                    status: TaskStatus::Working,
                    events: Vec::new(),
                    result: None,
                    error: None,
                }),
                done: Condvar::new(),
            }),
        }
    }

    /// Report progress. `fraction` is clamped to `[0, 1]` and to be no less
    /// than the previous report, so clients never see progress go backwards.
    /// Returns [`Cancelled`] if a cancel is pending (a free checkpoint).
    pub fn progress(&self, fraction: f64, message: impl Into<String>) -> Result<(), Cancelled> {
        let mut g = self.shared.inner.lock().expect("task lock");
        let last = g.events.last().map(|e| e.progress).unwrap_or(0.0);
        let f = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0).max(last)
        } else {
            last
        };
        g.events.push(ProgressEvent {
            progress: f,
            message: message.into(),
        });
        drop(g);
        self.checkpoint()
    }

    /// Cheap cancellation check for inner loops.
    pub fn checkpoint(&self) -> Result<(), Cancelled> {
        if self.shared.cancel.load(Ordering::SeqCst) {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    /// Whether a cancel was requested.
    pub fn is_cancelled(&self) -> bool {
        self.shared.cancel.load(Ordering::SeqCst)
    }

    /// All progress events so far.
    pub fn events(&self) -> Vec<ProgressEvent> {
        self.shared.inner.lock().expect("task lock").events.clone()
    }
}

/// The caller's side of a spawned task.
#[derive(Debug)]
pub struct TaskHandle {
    ctx: TaskCtx,
    join: Option<JoinHandle<()>>,
}

impl TaskHandle {
    /// Request cancellation (`tasks/cancel`). Idempotent; a task that already
    /// finished keeps its terminal state.
    pub fn cancel(&self) {
        self.ctx.shared.cancel.store(true, Ordering::SeqCst);
    }

    /// Current snapshot (`tasks/get`).
    pub fn snapshot(&self) -> TaskSnapshot {
        let g = self.ctx.shared.inner.lock().expect("task lock");
        let last = g.events.last();
        TaskSnapshot {
            status: g.status,
            progress: last.map(|e| e.progress).unwrap_or(0.0),
            message: last.map(|e| e.message.clone()).unwrap_or_default(),
            result: g.result.clone(),
            error: g.error.clone(),
        }
    }

    /// All progress events so far.
    pub fn events(&self) -> Vec<ProgressEvent> {
        self.ctx.events()
    }

    /// Block until the task is terminal and return the final snapshot
    /// (`tasks/result`).
    pub fn wait(mut self) -> TaskSnapshot {
        {
            let mut g = self.ctx.shared.inner.lock().expect("task lock");
            while !g.status.is_terminal() {
                g = self.ctx.shared.done.wait(g).expect("task lock");
            }
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        self.snapshot()
    }
}

/// Run `job` on its own thread as a task.
pub fn spawn<F>(job: F) -> TaskHandle
where
    F: FnOnce(&TaskCtx) -> Result<Value, JobError> + Send + 'static,
{
    let ctx = TaskCtx::detached();
    let worker = ctx.clone();
    let join = std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&worker)));
        let mut g = worker.shared.inner.lock().expect("task lock");
        match outcome {
            Ok(Ok(v)) => {
                g.status = TaskStatus::Completed;
                g.result = Some(v);
                let n = g.events.len();
                if n == 0 || g.events[n - 1].progress < 1.0 {
                    g.events.push(ProgressEvent {
                        progress: 1.0,
                        message: "done".into(),
                    });
                }
            }
            Ok(Err(JobError::Cancelled)) => g.status = TaskStatus::Cancelled,
            Ok(Err(JobError::Failed { message, report })) => {
                g.status = TaskStatus::Failed;
                g.error = Some(message);
                g.result = report;
            }
            Err(_) => {
                g.status = TaskStatus::Failed;
                g.error = Some("task panicked".into());
            }
        }
        drop(g);
        worker.shared.done.notify_all();
    });
    TaskHandle {
        ctx,
        join: Some(join),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn progress_is_monotone_and_clamped() {
        let ctx = TaskCtx::detached();
        ctx.progress(0.5, "a").unwrap();
        ctx.progress(0.2, "b").unwrap();
        ctx.progress(7.0, "c").unwrap();
        let p: Vec<f64> = ctx.events().iter().map(|e| e.progress).collect();
        assert_eq!(p, vec![0.5, 0.5, 1.0]);
    }

    #[test]
    fn completed_task_reports_result() {
        let h = spawn(|ctx| {
            ctx.progress(0.3, "work")?;
            Ok(json!({"ok": true}))
        });
        let s = h.wait();
        assert_eq!(s.status, TaskStatus::Completed);
        assert_eq!(s.result, Some(json!({"ok": true})));
        assert_eq!(s.progress, 1.0);
    }

    #[test]
    fn cancel_stops_a_cooperative_loop() {
        let h = spawn(|ctx| {
            for i in 0..1_000_000u64 {
                ctx.progress(i as f64 / 1e6, "spin")?;
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Ok(json!(null))
        });
        while h.events().len() < 3 {
            std::thread::yield_now();
        }
        h.cancel();
        let s = h.wait();
        assert_eq!(s.status, TaskStatus::Cancelled);
        assert!(s.result.is_none());
    }

    #[test]
    fn failure_keeps_the_report() {
        let h = spawn(|_| {
            Err(JobError::Failed {
                message: "gate".into(),
                report: Some(json!({"gate": "fits_bed", "pass": false})),
            })
        });
        let s = h.wait();
        assert_eq!(s.status, TaskStatus::Failed);
        assert_eq!(s.error.as_deref(), Some("gate"));
        assert_eq!(s.result.unwrap()["gate"], "fits_bed");
    }
}
