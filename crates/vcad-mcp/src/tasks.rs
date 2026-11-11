//! Process event bus + the Tasks extension store (SEP-2663).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use crate::protocol::ERR_INTERNAL;

/// Events fanned out to `subscriptions/listen` streams and legacy connections.
#[derive(Debug, Clone)]
pub enum BusEvent {
    /// The tool surface changed.
    ToolsListChanged,
    /// A task changed (detailed wire shape).
    Task(Value),
    /// A resource's content changed.
    ResourceUpdated(String),
    /// An agent job started, progressed or ended ([`crate::jobs`]).
    Job(Value),
}

type Listener = Arc<dyn Fn(&BusEvent) + Send + Sync>;

/// Process-scoped event bus.
#[derive(Default)]
pub struct Bus {
    next: AtomicU64,
    listeners: Mutex<HashMap<u64, Listener>>,
}

impl Bus {
    /// Register a listener; returns its id for [`Bus::off`].
    pub fn on(&self, f: Listener) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, f);
        id
    }

    /// Remove a listener.
    pub fn off(&self, id: u64) {
        self.listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }

    /// Deliver an event to every listener.
    pub fn emit(&self, evt: BusEvent) {
        let ls: Vec<Listener> = self
            .listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for l in ls {
            l(&evt);
        }
    }
}

/// Finished tasks stay retrievable this long.
pub const TASK_TTL_MS: u64 = 30 * 60_000;
/// Suggested poll cadence.
pub const TASK_POLL_INTERVAL_MS: u64 = 1_000;

/// RFC 3339 UTC timestamp (no chrono dependency).
pub fn now_iso() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60,
        d.subsec_millis()
    )
}

struct Record {
    status: &'static str,
    status_message: Option<String>,
    created_at: String,
    last_updated_at: String,
    expires_at: Instant,
    tool_name: String,
    input_requests: Map<String, Value>,
    pending: HashMap<String, Sender<Value>>,
    preset: Map<String, Value>,
    seq: u64,
    result: Option<Value>,
    error: Option<Value>,
    cancel: Arc<AtomicBool>,
}

impl Record {
    fn detailed(&self, id: &str) -> Value {
        let mut v = json!({
            "taskId": id,
            "status": self.status,
            "createdAt": self.created_at,
            "lastUpdatedAt": self.last_updated_at,
            "ttlMs": TASK_TTL_MS,
            "pollIntervalMs": TASK_POLL_INTERVAL_MS,
        });
        if let Some(m) = &self.status_message {
            v["statusMessage"] = Value::String(m.clone());
        }
        match self.status {
            "input_required" => v["inputRequests"] = Value::Object(self.input_requests.clone()),
            "completed" => v["result"] = self.result.clone().unwrap_or_else(|| json!({})),
            "failed" => {
                v["error"] = self
                    .error
                    .clone()
                    .unwrap_or_else(|| json!({"code": ERR_INTERNAL, "message": "unknown"}))
            }
            _ => {}
        }
        v
    }
}

/// In-memory task store with TTL.
pub struct TaskStore {
    tasks: Mutex<HashMap<String, Record>>,
    bus: Arc<Bus>,
}

/// Outcome of a task-method call: a JSON-RPC result or an error message.
pub type TaskReply = Result<Value, String>;

impl TaskStore {
    /// New store emitting on `bus`.
    pub fn new(bus: Arc<Bus>) -> Self {
        TaskStore {
            tasks: Mutex::new(HashMap::new()),
            bus,
        }
    }

    fn prune(map: &mut HashMap<String, Record>) {
        let now = Instant::now();
        map.retain(|_, r| r.expires_at > now);
    }

    fn touch(&self, id: &str, f: impl FnOnce(&mut Record)) {
        let detailed = {
            let mut map = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            let Some(r) = map.get_mut(id) else { return };
            f(r);
            r.last_updated_at = now_iso();
            r.detailed(id)
        };
        self.bus.emit(BusEvent::Task(detailed));
    }

    /// Create a `working` task; returns `(task_id, cancel flag, detailed)`.
    pub fn create(
        &self,
        tool_name: &str,
        preset: Map<String, Value>,
    ) -> (String, Arc<AtomicBool>, Value) {
        let mut raw = [0u8; 8];
        let _ = getrandom::getrandom(&mut raw);
        let id = format!(
            "task_{}",
            raw.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let now = now_iso();
        let cancel = Arc::new(AtomicBool::new(false));
        let rec = Record {
            status: "working",
            status_message: Some(format!("running {tool_name}")),
            created_at: now.clone(),
            last_updated_at: now,
            expires_at: Instant::now() + Duration::from_millis(TASK_TTL_MS),
            tool_name: tool_name.to_string(),
            input_requests: Map::new(),
            pending: HashMap::new(),
            preset,
            seq: 0,
            result: None,
            error: None,
            cancel: cancel.clone(),
        };
        let detailed = rec.detailed(&id);
        let mut map = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        Self::prune(&mut map);
        map.insert(id.clone(), rec);
        (id, cancel, detailed)
    }

    /// Progress milestone → `statusMessage` (only while working).
    pub fn progress(&self, id: &str, message: String) {
        self.touch(id, |r| {
            if r.status == "working" {
                r.status_message = Some(message);
            }
        });
    }

    /// The task finished with a tool result.
    pub fn complete(&self, id: &str, mut result: Value) {
        result["resultType"] = json!("complete");
        self.touch(id, |r| {
            if r.status == "cancelled" {
                return;
            }
            let name = r.tool_name.clone();
            r.status = "completed";
            r.status_message = Some(format!("{name} finished"));
            r.result = Some(result);
        });
    }

    /// The task failed with a JSON-RPC error object.
    pub fn fail(&self, id: &str, error: Value) {
        self.touch(id, |r| {
            if r.status == "cancelled" {
                return;
            }
            r.status = "failed";
            r.status_message = error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string);
            r.error = Some(error);
        });
    }

    /// Elicitation raised inside the task: park as `input_required` and block
    /// until `tasks/update` (or cancel) answers it.
    pub fn elicit(&self, id: &str, params: Value) -> Value {
        let rx = {
            let mut map = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            let Some(r) = map.get_mut(id) else {
                return json!({"action": "cancel"});
            };
            r.seq += 1;
            let key = format!("input_{}", r.seq);
            if let Some(p) = r.preset.get(&key) {
                return p.clone();
            }
            if r.cancel.load(Ordering::Relaxed) {
                return json!({"action": "cancel"});
            }
            let mut clean = params;
            if let Some(o) = clean.as_object_mut() {
                o.remove("elicitationId");
            }
            r.input_requests.insert(
                key.clone(),
                json!({"method": "elicitation/create", "params": clean}),
            );
            let (tx, rx) = channel();
            r.pending.insert(key, tx);
            rx
        };
        self.touch(id, |r| {
            r.status = "input_required";
            r.status_message = Some("waiting for client input".into());
        });
        rx.recv().unwrap_or_else(|_| json!({"action": "cancel"}))
    }

    /// `tasks/get` | `tasks/update` | `tasks/cancel`.
    pub fn handle(&self, method: &str, params: &Value) -> TaskReply {
        let task_id = params
            .get("taskId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        {
            let mut map = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            Self::prune(&mut map);
            if !map.contains_key(&task_id) {
                return Err(format!(
                    "Unknown taskId '{task_id}' — expired, cancelled long ago, or from a previous server instance"
                ));
            }
            if method == "tasks/get" {
                let mut v = map[&task_id].detailed(&task_id);
                v["resultType"] = json!("complete");
                return Ok(v);
            }
        }
        if method == "tasks/update" {
            let responses = params
                .get("inputResponses")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let resume = {
                let mut map = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
                let r = map.get_mut(&task_id).expect("checked above");
                for (key, resp) in responses {
                    if let Some(tx) = r.pending.remove(&key) {
                        r.input_requests.remove(&key);
                        let _ = tx.send(resp);
                    }
                }
                r.status == "input_required" && r.input_requests.is_empty()
            };
            if resume {
                self.touch(&task_id, |r| {
                    r.status = "working";
                    r.status_message = Some(format!("running {}", r.tool_name));
                });
            }
            return Ok(json!({"resultType": "complete"}));
        }
        // tasks/cancel — cooperative.
        self.touch(&task_id, |r| {
            r.cancel.store(true, Ordering::Relaxed);
            if r.status == "working" || r.status == "input_required" {
                for (_, tx) in r.pending.drain() {
                    let _ = tx.send(json!({"action": "cancel"}));
                }
                r.input_requests.clear();
                r.status = "cancelled";
                r.status_message = Some("cancelled by client".into());
            }
        });
        Ok(json!({"resultType": "complete"}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_shape() {
        let s = now_iso();
        assert_eq!(s.len(), 24);
        assert!(s.ends_with('Z') && s.as_bytes()[10] == b'T');
    }

    #[test]
    fn lifecycle() {
        let store = TaskStore::new(Arc::new(Bus::default()));
        let (id, _, d) = store.create("x", Map::new());
        assert_eq!(d["status"], "working");
        store.complete(&id, json!({"content": []}));
        let got = store.handle("tasks/get", &json!({"taskId": id})).unwrap();
        assert_eq!(got["status"], "completed");
        assert!(store
            .handle("tasks/get", &json!({"taskId": "nope"}))
            .is_err());
    }
}
