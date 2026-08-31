//! Owns the harness child process and the protocol streams over its stdio.
//!
//! The child's stdout carries protocol traffic only; its stderr carries the bridge's
//! structured log records and the harness host's own output. Both are normalized into
//! [`Incoming`] so one pane, and one file, hold the whole flow.
//!
//! Every frame is logged as it crosses, in both directions, tagged with its exchange id.
//! Calls and streams are timed here rather than by either peer: this is the only place
//! that sees both halves of an exchange, so it is the only place that can say how long
//! one took.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use dsh_tui_proto::{decode, encode, ClientMsg, ExchangeId, ServerMsg};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, Notify};

use crate::logging::{self, Level, Origin, Record};

/// Anything arriving from the child, normalized into one stream for the app loop.
#[derive(Debug)]
pub enum Incoming {
    /// A well-formed protocol message.
    Msg(Box<ServerMsg>),
    /// A log record: from the bridge, from the harness host, or about the transport
    /// itself. Already written to the log file; carried here for the in-app pane.
    Note(Record),
    /// A stdout line that did not parse. Logged and skipped: one corrupt frame must not
    /// desynchronize the stream.
    Malformed { line: String, error: String },
    /// The child exited. Nothing further will arrive.
    Exited { status: Option<i32> },
}

/// An exchange the TUI opened and is still waiting on.
struct Pending {
    started: Instant,
    /// `session.list` or `session.follow` — what the reply is a reply *to*, which the
    /// reply frame itself does not carry.
    label: String,
    /// Stream items seen so far. Zero on a call.
    items: u64,
}

/// Exchanges in flight, shared between the send path and the reader task.
type Inflight = Arc<Mutex<HashMap<ExchangeId, Pending>>>;

/// A running harness runtime and its protocol channels.
pub struct Transport {
    /// Fires once to ask the supervisor task to kill the child.
    kill: Option<oneshot::Sender<()>>,
    /// Signalled by the supervisor task when the child is actually gone.
    exited: Arc<Notify>,
    outgoing: mpsc::UnboundedSender<ClientMsg>,
    /// Lets the send path put its own log records in the app's stream, so the pane shows
    /// requests and replies interleaved in the order they actually happened.
    notes: mpsc::UnboundedSender<Incoming>,
    inflight: Inflight,
    next_id: ExchangeId,
}

impl Transport {
    /// Spawn `program` with `args` and wire up its stdio.
    ///
    /// Returns the transport and the receiver the app loop drains.
    pub fn spawn(
        program: &str,
        args: &[String],
    ) -> Result<(Self, mpsc::UnboundedReceiver<Incoming>)> {
        Self::spawn_with_env(program, args, &[])
    }

    /// Spawn with extra environment variables. Used by tests to steer the dev stub.
    pub fn spawn_with_env(
        program: &str,
        args: &[String],
        envs: &[(&str, &str)],
    ) -> Result<(Self, mpsc::UnboundedReceiver<Incoming>)> {
        let mut child = Command::new(program)
            .args(args)
            .envs(envs.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn harness runtime: {program}"))?;

        let pid = child.id();
        let stdin = child.stdin.take().context("child stdin was not piped")?;
        let stdout = child.stdout.take().context("child stdout was not piped")?;
        let stderr = child.stderr.take().context("child stderr was not piped")?;

        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        let exited = Arc::new(Notify::new());
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<ClientMsg>();
        let inflight: Inflight = Arc::new(Mutex::new(HashMap::new()));

        note(
            &incoming_tx,
            Record::info("transport.spawn")
                .msg(format!("spawned {program}"))
                .field("program", program)
                .field("args", Value::from(args.to_vec()))
                .field("pid", pid.map(Value::from).unwrap_or(Value::Null)),
        );

        // Writer: serialize client messages onto the child's stdin.
        let tx = incoming_tx.clone();
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(msg) = outgoing_rx.recv().await {
                let Ok(line) = encode(&msg) else {
                    note(
                        &tx,
                        Record::error("frame.encode")
                            .msg("a client message could not be encoded and was dropped"),
                    );
                    continue;
                };
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    note(
                        &tx,
                        Record::warn("frame.write")
                            .msg("the runtime stopped accepting input mid-write"),
                    );
                    break;
                }
                if stdin.flush().await.is_err() {
                    break;
                }
            }
        });

        // Reader: protocol frames.
        let tx = incoming_tx.clone();
        let reader_inflight = inflight.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let item = match decode::<ServerMsg>(&line) {
                    Ok(msg) => {
                        log_incoming(&tx, &reader_inflight, &msg, line.len());
                        Incoming::Msg(Box::new(msg))
                    }
                    Err(error) => Incoming::Malformed {
                        line,
                        error: error.to_string(),
                    },
                };
                if tx.send(item).is_err() {
                    break;
                }
            }
        });

        // Reader: the bridge's structured records, and whatever the host prints.
        let tx = incoming_tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let record = Record::parse_bridge_line(&line)
                    .unwrap_or_else(|| Record::from_raw_line(&line));
                note(&tx, record);
                if tx.is_closed() {
                    break;
                }
            }
        });

        // Supervisor: owns the child so its exit is observed exactly once, whether it
        // ends on its own or because the TUI asked for it.
        let notes = incoming_tx.clone();
        let tx = incoming_tx;
        let exit_signal = exited.clone();
        tokio::spawn(async move {
            let mut child = child;
            let status = tokio::select! {
                status = child.wait() => status.ok().and_then(|status| status.code()),
                _ = kill_rx => {
                    let _ = child.kill().await;
                    child.wait().await.ok().and_then(|status| status.code())
                }
            };
            let _ = tx.send(Incoming::Exited { status });
            exit_signal.notify_waiters();
        });

        Ok((
            Self {
                kill: Some(kill_tx),
                exited,
                outgoing: outgoing_tx,
                notes,
                inflight,
                next_id: 1,
            },
            incoming_rx,
        ))
    }

    /// Allocate the next client-originated exchange id.
    pub fn next_id(&mut self) -> ExchangeId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Queue a message for the child. Fails only once the child is gone.
    pub fn send(&self, msg: ClientMsg) -> Result<()> {
        self.log_outgoing(&msg);
        self.outgoing
            .send(msg)
            .context("harness runtime is no longer accepting messages")
    }

    /// Invoke a remote method, returning the id its reply will carry.
    pub fn call(
        &mut self,
        ns: &str,
        method: &str,
        args: serde_json::Value,
    ) -> Result<ExchangeId> {
        let id = self.next_id();
        self.begin(id, format!("{ns}.{method}"));
        self.send(ClientMsg::Call {
            id,
            ns: ns.to_string(),
            m: method.to_string(),
            args: Some(args),
        })?;
        Ok(id)
    }

    /// Open a stream, returning the id its items will carry.
    ///
    /// Used by the conversation pane for `session.follow` and `session.control`.
    #[allow(dead_code)]
    pub fn open(&mut self, stream: &str, args: serde_json::Value) -> Result<ExchangeId> {
        let id = self.next_id();
        self.begin(id, stream.to_string());
        self.send(ClientMsg::Open {
            id,
            stream: stream.to_string(),
            args: Some(args),
        })?;
        Ok(id)
    }

    /// Start the clock on an exchange, so its reply can be reported with a duration.
    fn begin(&self, id: ExchangeId, label: String) {
        if let Ok(mut inflight) = self.inflight.lock() {
            inflight.insert(
                id,
                Pending {
                    started: Instant::now(),
                    label,
                    items: 0,
                },
            );
        }
    }

    fn log_outgoing(&self, msg: &ClientMsg) {
        let record = match msg {
            ClientMsg::Call { id, ns, m, args } => {
                if !logging::enabled(Level::Debug) {
                    return;
                }
                Record::debug("call.out")
                    .exchange(Origin::Client, *id)
                    .msg(format!("→ {ns}.{m}"))
                    .field("ns", ns.clone())
                    .field("m", m.clone())
                    .payload("args", args.as_ref().unwrap_or(&Value::Null))
            }
            ClientMsg::Open { id, stream, args } => Record::info("stream.out")
                .exchange(Origin::Client, *id)
                .msg(format!("→ open {stream}"))
                .field("stream", stream.clone())
                .payload("args", args.as_ref().unwrap_or(&Value::Null)),
            ClientMsg::Cancel { id } => Record::debug("call.cancel").exchange(Origin::Client, *id).msg("→ cancel"),
            ClientMsg::Close { id } => Record::debug("stream.close").exchange(Origin::Client, *id).msg("→ close"),
            // The three waterfall replies are the moments a human unblocked the agent.
            // They are the most valuable rows in the file and are never filtered out.
            ClientMsg::Answer { id, v } => Record::info("ask.answer")
                .exchange(Origin::Bridge, *id)
                .msg("→ answered")
                .payload("v", v),
            ClientMsg::Next { id } => Record::info("ask.next")
                .exchange(Origin::Bridge, *id)
                .msg("→ delegated to the host"),
            ClientMsg::Reject { id, message } => Record::warn("ask.reject")
                .exchange(Origin::Bridge, *id)
                .msg(format!("→ rejected: {message}"))
                .field("message", message.clone()),
            ClientMsg::Shutdown => Record::info("shutdown.out").msg("→ shutdown"),
        };
        note(&self.notes, record);
    }

    /// Ask for a clean shutdown, then make sure the child is actually gone.
    ///
    /// The terminal is restored by the caller's guard regardless of what happens here.
    pub async fn shutdown(&mut self) {
        let exited = self.exited.clone();
        let notified = exited.notified();
        tokio::pin!(notified);

        let _ = self.send(ClientMsg::Shutdown);
        let grace = tokio::time::sleep(std::time::Duration::from_millis(1500));
        tokio::select! {
            _ = &mut notified => return,
            _ = grace => {}
        }

        // The runtime ignored the request or is wedged; take it down.
        note(
            &self.notes,
            Record::warn("shutdown.kill")
                .msg("the runtime did not exit within the grace period; killing it"),
        );
        if let Some(kill) = self.kill.take() {
            let _ = kill.send(());
        }
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), notified).await;
    }
}

/// Write a record to the log file, then put it in the app's stream for the pane.
///
/// A filtered record reaches neither, so the pane never claims to show a run the file
/// does not contain.
fn note(tx: &mpsc::UnboundedSender<Incoming>, record: Record) {
    if let Some(record) = logging::emit(record) {
        let _ = tx.send(Incoming::Note(record));
    }
}

/// Log one server frame, closing out the exchange it answers.
fn log_incoming(
    tx: &mpsc::UnboundedSender<Incoming>,
    inflight: &Inflight,
    msg: &ServerMsg,
    bytes: usize,
) {
    let record = match msg {
        ServerMsg::Ready(ready) => Record::info("ready")
            .msg(format!(
                "connected: {} namespaces, {} forwarded events",
                ready.namespaces.len(),
                ready.events.len()
            ))
            .field("protocol", ready.protocol)
            .field("namespaces", Value::from(ready.namespaces.clone()))
            .field("events", Value::from(ready.events.clone())),
        ServerMsg::Ok { id, v } => {
            if !logging::enabled(Level::Debug) {
                settle(inflight, *id);
                return;
            }
            let done = settle(inflight, *id);
            Record::debug("call.ok")
                .exchange(Origin::Client, *id)
                .msg(format!("← ok {}", done.label))
                .field("ms", done.millis)
                .field("bytes", bytes)
                .payload("v", v)
        }
        // A failure is worth a record whatever the threshold: this is the line someone
        // opens the log to find.
        ServerMsg::Err { id, code, message, data } => {
            let done = settle(inflight, *id);
            Record::warn("call.err")
                .exchange(Origin::Client, *id)
                .msg(format!("← err {} [{code}]: {message}", done.label))
                .field("code", code.clone())
                .field("message", message.clone())
                .field("ms", done.millis)
                .payload("data", data.as_ref().unwrap_or(&Value::Null))
        }
        ServerMsg::Item { id, gen, v } => {
            let count = count_item(inflight, *id);
            if !logging::enabled(Level::Trace) {
                return;
            }
            Record::trace("stream.item")
                .exchange(Origin::Client, *id)
                .msg(format!("← item #{count}"))
                .field("gen", *gen)
                .field("n", count)
                .field("bytes", bytes)
                .payload("v", v)
        }
        ServerMsg::End { id, reason } => {
            let done = settle(inflight, *id);
            Record::info("stream.end")
                .exchange(Origin::Client, *id)
                .msg(format!("← end {} ({})", done.label, reason.as_deref().unwrap_or("-")))
                .field("ms", done.millis)
                .field("items", done.items)
                .field("reason", reason.clone().unwrap_or_default())
        }
        ServerMsg::StreamErr { id, code, message } => {
            let done = settle(inflight, *id);
            Record::error("stream.err")
                .exchange(Origin::Client, *id)
                .msg(format!("← stream failed {} [{code}]: {message}", done.label))
                .field("code", code.clone())
                .field("message", message.clone())
                .field("ms", done.millis)
                .field("items", done.items)
        }
        ServerMsg::Event { event, args } => {
            if !logging::enabled(Level::Debug) {
                return;
            }
            Record::debug("event")
                .msg(format!("← {event}"))
                .field("event", event.clone())
                .field("bytes", bytes)
                .payload("args", &Value::Array(args.clone()))
        }
        // The agent is now blocked on the human. Always recorded, and the paired
        // `ask.answer`/`ask.next`/`ask.reject` carries the same id.
        ServerMsg::Ask { id, event, args, .. } => Record::info("ask")
            .exchange(Origin::Bridge, *id)
            .msg(format!("← ask {event} (agent blocked)"))
            .field("event", event.clone())
            .payload("args", &Value::Array(args.clone())),
        ServerMsg::Bye => Record::info("bye").msg("← bye"),
    };
    note(tx, record);
}

/// What an exchange cost, once its terminal frame arrives.
struct Settled {
    label: String,
    millis: u64,
    items: u64,
}

fn settle(inflight: &Inflight, id: ExchangeId) -> Settled {
    let pending = inflight.lock().ok().and_then(|mut map| map.remove(&id));
    match pending {
        Some(pending) => Settled {
            label: pending.label,
            millis: pending.started.elapsed().as_millis() as u64,
            items: pending.items,
        },
        // A reply with no request is not an error: the bridge originates waterfalls, and
        // a reconnect can outlive the table. Say so rather than inventing a duration.
        None => Settled {
            label: "(unknown)".to_string(),
            millis: 0,
            items: 0,
        },
    }
}

fn count_item(inflight: &Inflight, id: ExchangeId) -> u64 {
    let Ok(mut map) = inflight.lock() else { return 0 };
    match map.get_mut(&id) {
        Some(pending) => {
            pending.items += 1;
            pending.items
        }
        None => 0,
    }
}
