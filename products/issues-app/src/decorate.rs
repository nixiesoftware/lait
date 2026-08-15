//! Local presentation: authored names on a decoded Issues reply.
//!
//! The World stays name-free. This walks the decoded JSON, batches actor
//! ids the product already carried, asks [`ClientHost::call_identity`] once,
//! and adds optional fields beside unchanged ids. An unavailable book is
//! left alone — that is not "these people have no names".

use runtime::world::call::Call;
use serde_json::{Map, Value};
use world_interface::{ClientFuture, ClientHost, PresentationHandle, PresentationResolution};

/// Decorate one decoded Issues reply.
pub fn decorate_reply<'a>(
    host: &'a dyn ClientHost,
    _call: &'a Call,
    value: Value,
) -> ClientFuture<'a, Value> {
    Box::pin(async move {
        let actors = collect_actors(&value);
        if actors.is_empty() {
            return Ok(value);
        }
        let handles = actors
            .iter()
            .cloned()
            .map(PresentationHandle::actor)
            .collect();
        let resolution = match host.call_identity(handles).await {
            Ok(resolution) => resolution,
            Err(_) => return Ok(value),
        };
        if resolution.is_unavailable() {
            return Ok(value);
        }
        let names = names_by_actor(&resolution);
        if names.is_empty() {
            return Ok(value);
        }
        let mut decorated = value;
        apply_labels(&mut decorated, &names);
        Ok(decorated)
    })
}

fn is_actor(raw: &str) -> bool {
    let Some(hex) = raw.strip_prefix("act_") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

fn collect_actors(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk_collect(value, &mut out);
    out.sort();
    out.dedup();
    out
}

fn walk_collect(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            take_actor(map.get("actor"), out);
            take_actor(map.get("created_by"), out);
            take_actor_list(map.get("assignees"), out);
            take_actor_list(map.get("followers"), out);
            for nested in map.values() {
                walk_collect(nested, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                walk_collect(item, out);
            }
        }
        _ => {}
    }
}

fn take_actor(value: Option<&Value>, out: &mut Vec<String>) {
    if let Some(id) = value.and_then(Value::as_str).filter(|id| is_actor(id)) {
        out.push(id.to_owned());
    }
}

fn take_actor_list(value: Option<&Value>, out: &mut Vec<String>) {
    let Some(Value::Array(items)) = value else {
        return;
    };
    for item in items {
        take_actor(Some(item), out);
    }
}

fn names_by_actor(resolution: &PresentationResolution) -> Map<String, Value> {
    let mut names = Map::new();
    for label in &resolution.labels {
        let PresentationHandle::Actor { actor, .. } = &label.handle else {
            continue;
        };
        let Some(name) = label.name.as_deref().filter(|name| !name.is_empty()) else {
            continue;
        };
        names.insert(actor.clone(), Value::String(name.to_owned()));
    }
    names
}

fn apply_labels(value: &mut Value, names: &Map<String, Value>) {
    match value {
        Value::Object(map) => {
            if let Some(id) = map.get("actor").and_then(Value::as_str) {
                if let Some(name) = names.get(id) {
                    map.insert("authored_name".into(), name.clone());
                }
            }
            if let Some(id) = map.get("created_by").and_then(Value::as_str) {
                if let Some(name) = names.get(id) {
                    map.insert("created_by_name".into(), name.clone());
                }
            }
            attach_map(map, "assignees", "assignee_names", names);
            attach_map(map, "followers", "follower_names", names);
            for nested in map.values_mut() {
                apply_labels(nested, names);
            }
        }
        Value::Array(items) => {
            for item in items {
                apply_labels(item, names);
            }
        }
        _ => {}
    }
}

fn attach_map(map: &mut Map<String, Value>, source: &str, dest: &str, names: &Map<String, Value>) {
    let Some(Value::Array(items)) = map.get(source) else {
        return;
    };
    let mut labels = Map::new();
    for item in items {
        if let Some(id) = item.as_str() {
            if let Some(name) = names.get(id) {
                labels.insert(id.to_owned(), name.clone());
            }
        }
    }
    if !labels.is_empty() {
        map.insert(dest.into(), Value::Object(labels));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use replica::body::WorldId;
    use runtime::world::call::Call;
    use std::path::Path;
    use std::sync::Mutex;
    use world_interface::{
        ClientFuture, Failure, HostContentRequest, HostControlRequest, PresentationLabel,
    };

    const ADA: &str = "act_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct FakeHost {
        identity: PresentationResolution,
        calls: Mutex<usize>,
    }

    impl ClientHost for FakeHost {
        fn local_root(&self) -> &Path {
            Path::new(".")
        }

        fn call_world<'a>(&'a self, _call: Call) -> ClientFuture<'a, runtime::world::call::Reply> {
            Box::pin(async { Err(Failure::refusal()) })
        }

        fn call_control<'a>(&'a self, _request: HostControlRequest) -> ClientFuture<'a, Value> {
            Box::pin(async { Err(Failure::refusal()) })
        }

        fn call_work<'a>(
            &'a self,
            _request: runtime::exec::WorkRequest,
        ) -> ClientFuture<'a, Value> {
            Box::pin(async { Err(Failure::refusal()) })
        }

        fn call_content<'a>(&'a self, _request: HostContentRequest) -> ClientFuture<'a, Value> {
            Box::pin(async { Err(Failure::refusal()) })
        }

        fn call_identity<'a>(
            &'a self,
            _handles: Vec<PresentationHandle>,
        ) -> ClientFuture<'a, PresentationResolution> {
            *self.calls.lock().expect("calls") += 1;
            let identity = self.identity.clone();
            Box::pin(async move { Ok(identity) })
        }
    }

    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        let mut fut = std::pin::pin!(fut);
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(&waker);
        match fut.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => panic!("test host must not pend"),
        }
    }

    fn call() -> Call {
        Call::new(
            WorldId::parse("com.lait.issues").unwrap(),
            "issues.control",
            2,
            vec![],
        )
        .unwrap()
    }

    #[test]
    fn unavailable_leaves_the_decoded_json_alone() {
        let host = FakeHost {
            identity: PresentationResolution::unavailable(),
            calls: Mutex::new(0),
        };
        let input = serde_json::json!({
            "actor": ADA,
            "actor_nick": "",
            "assignees": [ADA],
        });
        let out = block_on(decorate_reply(&host, &call(), input.clone())).unwrap();
        assert_eq!(out, input);
        assert_eq!(*host.calls.lock().expect("calls"), 1);
    }

    #[test]
    fn a_live_hit_adds_optional_fields_and_keeps_the_ids() {
        let host = FakeHost {
            identity: PresentationResolution {
                labels: vec![PresentationLabel {
                    handle: PresentationHandle::actor(ADA),
                    name: Some("Ada".into()),
                }],
                coverage: None,
            },
            calls: Mutex::new(0),
        };
        let out = block_on(decorate_reply(
            &host,
            &call(),
            serde_json::json!({
                "actor": ADA,
                "created_by": ADA,
                "assignees": [ADA],
                "actor_nick": "",
            }),
        ))
        .unwrap();
        assert_eq!(out["actor"], ADA);
        assert_eq!(out["created_by"], ADA);
        assert_eq!(out["assignees"][0], ADA);
        assert_eq!(out["actor_nick"], "");
        assert_eq!(out["authored_name"], "Ada");
        assert_eq!(out["created_by_name"], "Ada");
        assert_eq!(out["assignee_names"][ADA], "Ada");
        assert_eq!(*host.calls.lock().expect("calls"), 1);
    }

    #[test]
    fn a_reply_with_no_actors_does_not_ask_the_book() {
        let host = FakeHost {
            identity: PresentationResolution::unavailable(),
            calls: Mutex::new(0),
        };
        let input = serde_json::json!({"title": "ENG-1", "key_alias": "ENG-1"});
        let out = block_on(decorate_reply(&host, &call(), input.clone())).unwrap();
        assert_eq!(out, input);
        assert_eq!(*host.calls.lock().expect("calls"), 0);
    }
}
