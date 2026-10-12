//! Serialized requests to the Yazi actor: one `ya emit-to` at a time, then a bounded poll for
//! its reply file. Nothing here blocks; the surface loop calls `poll` every tick.

use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::input::{Reply, Then};
use crate::inbox::{Inbox, STATE_CAP};

const REPLY_POLL: Duration = Duration::from_millis(50);
const REPLY_ATTEMPTS: u32 = 200;

struct Job {
    id: String,
    body: Value,
    then: Then,
}

enum Stage {
    Sending(Child),
    Waiting { attempts: u32, next: Instant },
}

pub(crate) struct Actions {
    cid: String,
    prefix: String,
    seq: u64,
    queue: VecDeque<Job>,
    flight: Option<(Job, Stage)>,
}

/// Quotes the JSON as one shell word: Yazi parses plugin args shell-style a second time.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn reply(value: &Value, id: &str) -> Option<Reply> {
    (value.get("id")?.as_str()? == id).then(|| Reply {
        ok: value.get("ok").and_then(Value::as_bool) == Some(true),
        rejected: value.get("rejected").and_then(Value::as_bool) == Some(true),
        error: value
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_owned),
        paths: value
            .get("paths")
            .and_then(Value::as_array)
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(|p| p.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn failure(error: impl Into<String>) -> Reply {
    Reply {
        error: Some(error.into()),
        ..Reply::default()
    }
}

impl Actions {
    pub(crate) fn new(cid: &str, token: &str) -> Self {
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        Self {
            cid: cid.to_owned(),
            // Request ids carry the session token so the inbox cleanup owns their replies.
            prefix: format!("{cid}-{token}-{epoch}-"),
            seq: 0,
            queue: VecDeque::new(),
            flight: None,
        }
    }

    pub(crate) fn push(&mut self, mut body: Value, then: Then) {
        self.seq += 1;
        let id = format!("{}{}", self.prefix, self.seq);
        body["id"] = id.clone().into();
        self.queue.push_back(Job { id, body, then });
    }

    /// Whether a filter edit is still on its way; until then the local query is authoritative.
    pub(crate) fn pending_filter(&self) -> bool {
        self.flight
            .iter()
            .map(|(job, _)| job)
            .chain(&self.queue)
            .any(|job| job.body["op"] == "filter")
    }

    /// Drops queued requests after a failure left Yazi's state unknown.
    pub(crate) fn clear(&mut self) {
        self.queue.clear();
    }

    /// Advances the request in flight; returns its continuation once Yazi answered.
    pub(crate) fn poll(&mut self, inbox: &Inbox) -> Option<(Then, Reply)> {
        if self.flight.is_none() {
            let job = self.queue.pop_front()?;
            let mut command = Command::new("ya");
            command
                .args(["emit-to", &self.cid, "plugin", "tern"])
                .arg(quote(&job.body.to_string()));
            // An async plugin run is itself a Yazi task, which would shift the task manager's
            // cursor under `inspect` and `cancel`; these run on Yazi's main thread instead.
            if job.body["op"] == "tasks" {
                command.arg("--mode=sync");
            }
            let stage = match command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(child) => Stage::Sending(child),
                Err(error) => return Some((job.then, failure(format!("cannot run ya: {error}")))),
            };
            self.flight = Some((job, stage));
        }
        let (job, stage) = self.flight.as_mut()?;
        let name = format!("reply-{}.json", job.id);
        let answer = match stage {
            Stage::Sending(child) => match child.try_wait() {
                Ok(None) => None,
                Ok(Some(status)) if status.success() => {
                    *stage = Stage::Waiting {
                        attempts: 0,
                        next: Instant::now(),
                    };
                    None
                }
                Ok(Some(_)) => {
                    let mut stderr = String::new();
                    if let Some(mut pipe) = child.stderr.take() {
                        let _ = pipe.read_to_string(&mut stderr);
                    }
                    Some(failure(stderr.trim().to_owned()))
                }
                Err(error) => Some(failure(error.to_string())),
            },
            Stage::Waiting { attempts, next } => {
                let now = Instant::now();
                if now < *next {
                    None
                } else if let Some(reply) = inbox
                    .read(&name, STATE_CAP, false)
                    .ok()
                    .and_then(|(bytes, _)| serde_json::from_slice::<Value>(&bytes).ok())
                    .and_then(|value| reply(&value, &job.id))
                {
                    inbox.remove(&name);
                    Some(reply)
                } else {
                    *attempts += 1;
                    *next = now + REPLY_POLL;
                    (*attempts >= REPLY_ATTEMPTS).then(|| {
                        failure("Timed out waiting for Yazi; outcome unknown, do not retry destructive actions blindly")
                    })
                }
            }
        }?;
        let (job, _) = self.flight.take()?;
        Some((job.then, answer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quotes_json_for_the_second_shell_parse() {
        assert_eq!(quote(r#"{"name":"it's"}"#), r#"'{"name":"it'\''s"}'"#);
    }

    #[test]
    fn replies_must_echo_the_request_id() {
        let value = json!({"id": "a-1", "ok": true, "paths": ["/x", 3]});
        let parsed = reply(&value, "a-1").unwrap();
        assert!(parsed.ok && !parsed.rejected);
        assert_eq!(parsed.paths, ["/x"]);
        assert!(reply(&value, "a-2").is_none());
        let rejected = reply(
            &json!({"id": "a-1", "ok": false, "rejected": true, "error": "no"}),
            "a-1",
        )
        .unwrap();
        assert!(!rejected.ok && rejected.rejected && rejected.error.as_deref() == Some("no"));
    }

    #[test]
    fn ids_are_sequential_and_filters_are_tracked() {
        let mut actions = Actions::new("12345", "tok");
        actions.push(json!({"op": "cd", "target": "/"}), Then::Nothing);
        actions.push(json!({"op": "filter", "query": "a"}), Then::Nothing);
        let ids: Vec<_> = actions.queue.iter().map(|job| job.id.clone()).collect();
        assert!(ids[0].starts_with("12345-tok-") && ids[0].ends_with("-1"));
        assert!(ids[1].ends_with("-2"));
        assert!(ids
            .iter()
            .all(|id| id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')));
        assert!(actions.pending_filter());
        actions.clear();
        assert!(!actions.pending_filter() && actions.queue.is_empty());
    }
}
