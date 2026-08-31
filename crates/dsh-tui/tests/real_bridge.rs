//! Tests against the compiled bridge server rather than the dev stub.
//!
//! The stub reimplements the wire; these exercise `BridgeServer`'s own dispatch, framing,
//! cancellation, and waterfall bookkeeping. Only the harness client face is faked.

use std::time::Duration;

use dsh_tui::app::{App, AskReply, Connection};
use dsh_tui::theme::Theme;
use dsh_tui::transport::{Incoming, Transport};
use dsh_tui_proto::{ClientMsg, ServerMsg};

fn server() -> (String, Vec<String>) {
    let path = format!("{}/../../bridge/test-server.mjs", env!("CARGO_MANIFEST_DIR"));
    ("node".to_string(), vec![path])
}

async fn drive(
    app: &mut App,
    transport: &mut Transport,
    incoming: &mut tokio::sync::mpsc::UnboundedReceiver<Incoming>,
    done: impl Fn(&App) -> bool,
) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if done(app) {
            return true;
        }
        match tokio::time::timeout_at(deadline, incoming.recv()).await {
            Ok(Some(msg)) => app.on_incoming(msg, transport),
            _ => break,
        }
    }
    done(app)
}

#[tokio::test]
async fn the_real_server_completes_a_handshake_and_answers_a_call() {
    let (program, args) = server();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("server should spawn");
    let mut app = App::new(Theme::default());

    let ok = drive(&mut app, &mut transport, &mut incoming, |app| {
        matches!(app.connection, Connection::Ready(_)) && !app.sessions.is_empty()
    })
    .await;
    assert!(ok, "expected the real bridge server to answer session.list");
    assert_eq!(app.sessions[0].title, "Real bridge server");
    transport.shutdown().await;
}

#[tokio::test]
async fn an_unknown_method_returns_its_error_code_not_a_generic_failure() {
    let (program, args) = server();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("server should spawn");

    let id = transport
        .call("session", "nope", serde_json::json!({}))
        .expect("send");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut seen = None;
    while let Ok(Some(msg)) = tokio::time::timeout_at(deadline, incoming.recv()).await {
        if let Incoming::Msg(msg) = msg {
            if let ServerMsg::Err { id: got, code, .. } = *msg {
                if got == id {
                    seen = Some(code);
                    break;
                }
            }
        }
    }
    // A thrown error carrying a code must keep it, so a policy rejection stays
    // distinguishable from a generic internal failure.
    assert_eq!(seen.as_deref(), Some("not-found"));
    transport.shutdown().await;
}

#[tokio::test]
async fn cancelling_a_call_aborts_it_on_the_server() {
    let (program, args) = server();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("server should spawn");

    let id = transport
        .call("session", "slow", serde_json::json!({}))
        .expect("send");
    transport.send(ClientMsg::Cancel { id }).expect("cancel");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut code = None;
    while let Ok(Some(msg)) = tokio::time::timeout_at(deadline, incoming.recv()).await {
        if let Incoming::Msg(msg) = msg {
            if let ServerMsg::Err { id: got, code: c, .. } = *msg {
                if got == id {
                    code = Some(c);
                    break;
                }
            }
        }
    }
    // Without the AbortSignal reaching the backend this would sit for five seconds.
    assert_eq!(code.as_deref(), Some("cancelled"));
    transport.shutdown().await;
}

#[tokio::test]
async fn closing_a_stream_ends_it() {
    let (program, args) = server();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("server should spawn");

    let id = transport
        .open("session.follow", serde_json::json!({ "sessionId": "s-1" }))
        .expect("open");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut items = 0;
    let mut ended = false;
    while let Ok(Some(msg)) = tokio::time::timeout_at(deadline, incoming.recv()).await {
        if let Incoming::Msg(msg) = msg {
            match *msg {
                ServerMsg::Item { id: got, .. } if got == id => {
                    items += 1;
                    if items == 2 {
                        transport.send(ClientMsg::Close { id }).expect("close");
                    }
                }
                ServerMsg::End { id: got, .. } if got == id => {
                    ended = true;
                    break;
                }
                _ => {}
            }
        }
    }
    assert_eq!(items, 2);
    assert!(ended, "close must produce an end frame");
    transport.shutdown().await;
}

#[tokio::test]
async fn delegating_a_waterfall_reaches_the_host_fallback() {
    let (program, args) = server();
    let (mut transport, mut incoming) =
        Transport::spawn_with_env(&program, &args, &[("DSH_TUI_TEST_ASK", "1")])
            .expect("server should spawn");
    let mut app = App::new(Theme::default());

    let arrived = drive(&mut app, &mut transport, &mut incoming, |app| !app.asks.is_empty()).await;
    assert!(arrived, "expected the waterfall to arrive");

    app.answer_ask(AskReply::Delegate, &mut transport);
    let settled = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.log.iter().any(|line| line.contains("DELEGATED"))
    })
    .await;
    // The server's `next()` produced the outcome, proving delegation ran the host
    // fallback rather than resolving with a value the TUI invented.
    assert!(settled, "expected next() to supply the outcome: {:?}", app.log);
    transport.shutdown().await;
}
