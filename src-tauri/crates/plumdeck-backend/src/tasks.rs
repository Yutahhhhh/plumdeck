//! `BackgroundTaskService` (backend/app/services/background_task_service.py,
//! S8): one task at a time, a JSON state with a revision counter, and every
//! change broadcast to the WebSocket clients (each gets the whole state on
//! connect).

use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// Shared task state and its subscribers.
#[derive(Clone)]
pub struct BackgroundTask {
    inner: Arc<Mutex<Inner>>,
    updates: broadcast::Sender<Value>,
}

struct Inner {
    state: Map<String, Value>,
    running: bool,
    handle: Option<JoinHandle<()>>,
}

fn idle_state() -> Map<String, Value> {
    let Value::Object(map) = json!({
        "type": "idle", "total": 0, "current": 0, "message": "", "processed": 0, "skipped": 0,
        "errors": 0, "start_time": 0, "estimated_remaining": 0, "run_id": null, "revision": 0, "details": {}
    }) else { unreachable!() };
    map
}

fn epoch_seconds() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

impl Default for BackgroundTask {
    fn default() -> Self {
        Self::new(Map::new())
    }
}

impl BackgroundTask {
    /// A service whose state carries `extra` top-level fields.
    pub fn new(extra: Map<String, Value>) -> Self {
        let mut state = idle_state();
        state.extend(extra);
        let (updates, _) = broadcast::channel(256);
        Self { inner: Arc::new(Mutex::new(Inner { state, running: false, handle: None })), updates }
    }

    pub fn state(&self) -> Value {
        Value::Object(self.inner.lock().expect("task state").state.clone())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.updates.subscribe()
    }

    pub fn is_running(&self) -> bool {
        self.inner.lock().expect("task state").running
    }

    /// `emit_state`.
    pub fn emit(&self) {
        let _ = self.updates.send(self.state());
    }

    /// `update_state`: known keys at the top, others under `details`;
    /// bumps the revision and recomputes the ETA when counts change.
    pub fn update(&self, changes: &[(&str, Value)]) {
        let mut inner = self.inner.lock().expect("task state");
        let state = &mut inner.state;
        let mut counts = false;
        for (key, value) in changes {
            if state.contains_key(*key) {
                state.insert((*key).to_owned(), value.clone());
            } else if let Some(Value::Object(details)) = state.get_mut("details") {
                details.insert((*key).to_owned(), value.clone());
            }
            counts |= matches!(*key, "processed" | "skipped" | "errors");
        }
        let revision = state.get("revision").and_then(Value::as_i64).unwrap_or(0) + 1;
        state.insert("revision".into(), json!(revision));
        if counts {
            let number = |key: &str| state.get(key).and_then(Value::as_f64).unwrap_or(0.0);
            let done = number("processed") + number("skipped") + number("errors");
            let (start, total) = (number("start_time"), number("total"));
            if start > 0.0 && done > 0.0 && total > 0.0 {
                let remaining = (total - done).max(0.0);
                state.insert("estimated_remaining".into(), json!((epoch_seconds() - start) / done * remaining));
            }
        }
    }

    /// Add `amount` to a counter (`self.state["updated"] += 1`), without a revision bump.
    pub fn add(&self, key: &str, amount: i64) {
        let mut inner = self.inner.lock().expect("task state");
        let value = inner.state.get(key).and_then(Value::as_i64).unwrap_or(0) + amount;
        inner.state.insert(key.to_owned(), json!(value));
    }

    /// `start_task`: `false` when a task is already running.
    pub fn start<F>(&self, work: impl FnOnce(BackgroundTask) -> F) -> bool
    where
        F: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        {
            let mut inner = self.inner.lock().expect("task state");
            if inner.running {
                return false;
            }
            inner.running = true;
            let state = &mut inner.state;
            for (key, value) in [
                ("type", json!("start")),
                ("run_id", json!(uuid::Uuid::new_v4().simple().to_string())),
                ("revision", json!(0)),
                ("start_time", json!(epoch_seconds())),
                ("processed", json!(0)),
                ("skipped", json!(0)),
                ("errors", json!(0)),
                ("current", json!(0)),
                ("total", json!(0)),
                ("estimated_remaining", json!(0)),
                ("message", json!("")),
                ("details", json!({})),
            ] {
                state.insert(key.into(), value);
            }
        }
        let future = work(self.clone());
        let me = self.clone();
        let handle = tokio::spawn(async move {
            let outcome = future.await;
            let kind = me.state().get("type").and_then(Value::as_str).unwrap_or_default().to_owned();
            match outcome {
                Ok(()) if !["error", "cancelled", "complete"].contains(&kind.as_str()) => {
                    me.update(&[("type", json!("complete")), ("message", json!("Task completed"))]);
                    me.emit();
                }
                Ok(()) => {}
                Err(message) => {
                    me.update(&[("type", json!("error")), ("message", json!(message))]);
                    me.emit();
                }
            }
            let mut inner = me.inner.lock().expect("task state");
            inner.running = false;
            inner.handle = None;
        });
        self.inner.lock().expect("task state").handle = Some(handle);
        self.emit();
        true
    }

    /// `cancel_task`.
    pub async fn cancel(&self) {
        let handle = self.inner.lock().expect("task state").handle.take();
        if let Some(handle) = handle {
            handle.abort();
            let _ = handle.await;
            {
                let mut inner = self.inner.lock().expect("task state");
                inner.running = false;
            }
            if self.state().get("type").and_then(Value::as_str) != Some("cancelled") {
                self.update(&[("type", json!("cancelled"))]);
                self.emit();
            }
        }
    }
}
