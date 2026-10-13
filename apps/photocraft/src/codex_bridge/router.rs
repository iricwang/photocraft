use serde_json::{Value, json};
use std::collections::HashMap;

pub enum Action {
    Backend(Value),
    Client(usize, Value),
    Broadcast(Value),
}

struct Pending {
    client: usize,
    id: Value,
    method: String,
    thread: Option<String>,
}

#[derive(Default)]
pub struct Router {
    serial: u64,
    pending: HashMap<String, Pending>,
    server_requests: HashMap<String, usize>,
    active: HashMap<String, usize>,
    owners: HashMap<String, usize>,
    initialized: Option<Value>,
}

fn thread_id(v: &Value) -> Option<String> {
    v.get("threadId").or_else(|| v.get("thread_id")).and_then(Value::as_str).or_else(|| v.pointer("/thread/id").and_then(Value::as_str)).map(str::to_owned)
}

fn reject(client: usize, id: Value, message: &str) -> Vec<Action> {
    vec![Action::Client(client, json!({"id":id,"error":{"code":-32001,"message":message}}))]
}

fn valid_id(id: &Value) -> bool {
    id.is_number() || id.as_str().is_some_and(|s| s.len() <= 256)
}

impl Router {
    pub fn client(&mut self, client: usize, mut v: Value) -> Vec<Action> {
        let Some(method) = v.get("method").and_then(Value::as_str).map(str::to_owned) else {
            if let Some(id) = v.get("id") {
                let key = id.to_string();
                if self.server_requests.get(&key) == Some(&client) {
                    self.server_requests.remove(&key);
                    return vec![Action::Backend(v)];
                }
            }
            return vec![];
        };
        let Some(id) = v.get("id").cloned() else {
            return if client == 0 { vec![Action::Backend(v)] } else { vec![] };
        };
        if !valid_id(&id) || method.len() > 256 {
            return reject(client, Value::Null, "Invalid shared connection request identifier or method");
        }
        if method == "initialize" && client != 0 {
            return match &self.initialized {
                Some(result) => vec![Action::Client(client, json!({"id":id,"result":result}))],
                None => reject(client, id, "Codex desktop shared connection is not initialized yet; no work was started."),
            };
        }
        if self.pending.len() >= 256 {
            return reject(client, id, "Shared connection request limit reached; no work was started.");
        }
        let thread = v.get("params").and_then(thread_id);
        if thread.as_ref().is_some_and(|s| s.len() > 256) {
            return reject(client, id, "Invalid shared connection thread identifier");
        }
        if let Some(thread) = &thread {
            let mutation = method.starts_with("turn/")
                || (method.starts_with("thread/") && !matches!(method.as_str(), "thread/read" | "thread/list" | "thread/loaded/list"));
            if let Some(owner) = self.active.get(thread)
                && ((mutation && *owner != client) || method == "turn/start")
            {
                return reject(
                    client,
                    id,
                    "This design conversation already has an active turn. Wait for it to finish; the same conversation is retained and nothing was retried.",
                );
            }
            if method == "turn/start" {
                self.active.insert(thread.clone(), client);
                self.owners.insert(thread.clone(), client);
            }
        }
        self.serial = self.serial.wrapping_add(1);
        let key = format!("photocraft-bridge-{}", self.serial);
        self.pending.insert(key.clone(), Pending { client, id, method, thread });
        v["id"] = Value::String(key);
        vec![Action::Backend(v)]
    }

    pub fn backend(&mut self, mut v: Value) -> Vec<Action> {
        if v.get("method").is_none() {
            let Some(key) = v.get("id").and_then(Value::as_str) else { return vec![] };
            let Some(pending) = self.pending.remove(key) else { return vec![] };
            if pending.method == "initialize" {
                self.initialized = v.get("result").cloned();
            }
            if matches!(pending.method.as_str(), "thread/resume" | "thread/read")
                && let Some(thread) = v.pointer("/result/thread")
            {
                let id = thread.get("id").and_then(Value::as_str).map(str::to_owned).or(pending.thread.clone());
                if let Some(id) = id
                    && (thread.pointer("/status/type").and_then(Value::as_str) == Some("active")
                        || thread
                            .get("turns")
                            .and_then(Value::as_array)
                            .is_some_and(|turns| turns.iter().any(|turn| turn.get("status").and_then(Value::as_str) == Some("inProgress"))))
                {
                    self.active.entry(id).or_insert(0);
                }
            }
            if pending.method == "turn/start"
                && v.get("error").is_some()
                && let Some(thread) = &pending.thread
            {
                self.active.remove(thread);
            }
            v["id"] = pending.id;
            return vec![Action::Client(pending.client, v)];
        }
        let thread = v.get("params").and_then(thread_id);
        if let Some(id) = v.get("id") {
            if !valid_id(id) || self.server_requests.len() >= 256 {
                return vec![Action::Backend(json!({"id":id,"error":{"code":-32001,"message":"Shared connection approval request limit reached"}}))];
            }
            let client = thread.as_ref().and_then(|t| self.active.get(t).or_else(|| self.owners.get(t))).copied().unwrap_or(0);
            self.server_requests.insert(id.to_string(), client);
            return vec![Action::Client(client, v)];
        }
        let method = v.get("method").and_then(Value::as_str).unwrap_or("");
        if method == "turn/started"
            && let Some(t) = &thread
        {
            self.active.entry(t.clone()).or_insert(0);
        }
        if method == "thread/status/changed"
            && v.pointer("/params/status/type").and_then(Value::as_str) == Some("active")
            && let Some(t) = &thread
        {
            self.active.entry(t.clone()).or_insert(0);
        }
        if method == "turn/completed"
            && let Some(t) = &thread
        {
            self.active.remove(t);
        }
        vec![Action::Broadcast(v)]
    }

    // A disconnected caller must not free an in-flight generation reservation.
    pub fn disconnected(&mut self, client: usize) {
        self.owners.retain(|_, owner| *owner != client);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn backend(actions: Vec<Action>) -> Value {
        match actions.into_iter().next().unwrap() {
            Action::Backend(v) => v,
            _ => panic!("expected backend"),
        }
    }
    #[test]
    fn ids_and_responses_are_isolated() {
        let mut r = Router::default();
        let a = backend(r.client(0, json!({"id":1,"method":"thread/list"})));
        let b = backend(r.client(1, json!({"id":1,"method":"thread/list"})));
        assert_ne!(a["id"], b["id"]);
        assert!(matches!(&r.backend(json!({"id":b["id"],"result":{}}))[0],Action::Client(1,v) if v["id"]==1));
    }
    #[test]
    fn reserve_before_response_and_keep_after_disconnect() {
        let mut r = Router::default();
        backend(r.client(1, json!({"id":1,"method":"turn/start","params":{"threadId":"same"}})));
        r.disconnected(1);
        assert!(matches!(&r.client(0,json!({"id":2,"method":"turn/start","params":{"threadId":"same"}}))[0],Action::Client(0,v) if v.get("error").is_some()));
        r.backend(json!({"method":"turn/completed","params":{"threadId":"same"}}));
        backend(r.client(0, json!({"id":3,"method":"turn/start","params":{"threadId":"same"}})));
    }
    #[test]
    fn active_other_owner_cannot_interrupt_but_owner_can_steer() {
        let mut r = Router::default();
        backend(r.client(0, json!({"id":1,"method":"turn/start","params":{"threadId":"same"}})));
        assert!(matches!(&r.client(1, json!({"id":2,"method":"turn/interrupt","params":{"threadId":"same"}}))[0], Action::Client(1, _)));
        backend(r.client(0, json!({"id":3,"method":"turn/steer","params":{"threadId":"same"}})));
    }
    #[test]
    fn secondary_initialize_does_not_initialize_backend_again() {
        let mut r = Router::default();
        let init = backend(r.client(0, json!({"id":1,"method":"initialize"})));
        r.backend(json!({"id":init["id"],"result":{"userAgent":"test"}}));
        assert!(matches!(&r.client(1,json!({"id":1,"method":"initialize"}))[0],Action::Client(1,v) if v["result"]["userAgent"]=="test"));
        assert!(r.client(1, json!({"method":"initialized"})).is_empty());
    }

    #[test]
    fn resumed_active_thread_is_not_owned_by_resuming_client() {
        let mut r = Router::default();
        let resume = backend(r.client(1, json!({"id":1,"method":"thread/resume","params":{"threadId":"same"}})));
        r.backend(json!({"id":resume["id"],"result":{"thread":{"id":"same","status":{"type":"active"}}}}));
        for method in ["turn/start", "turn/steer", "turn/interrupt", "thread/rollback", "thread/resume"] {
            assert!(matches!(&r.client(1,json!({"id":2,"method":method,"params":{"threadId":"same"}}))[0],Action::Client(1,v) if v.get("error").is_some()));
        }
        backend(r.client(0, json!({"id":3,"method":"turn/steer","params":{"threadId":"same"}})));
    }
}
