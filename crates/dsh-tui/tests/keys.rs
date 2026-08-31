//! Key-routing tests.
//!
//! These press keys the way a terminal does, through `keys::on_key`, rather than calling
//! the `App` method a binding is supposed to reach. That distinction is the whole point:
//! every binding here was already implemented and reachable by direct call when `^w`'s
//! arrows shipped broken — what was wrong was the routing, and only a keystroke sees it.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dsh_tui::app::{App, View};
use dsh_tui::keys::on_key;
use dsh_tui::theme::Theme;
use dsh_tui::transport::{Incoming, Transport};

fn stub() -> (String, Vec<String>) {
    let path = format!("{}/../../bridge/dev-stub.mjs", env!("CARGO_MANIFEST_DIR"));
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

fn press(app: &mut App, transport: &mut Transport, code: KeyCode) {
    on_key(app, transport, KeyEvent::new(code, KeyModifiers::NONE));
}

fn ctrl(app: &mut App, transport: &mut Transport, c: char) {
    on_key(app, transport, KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
}

/// A ready app with sessions and the workspace list already streamed in.
async fn ready() -> (App, Transport, tokio::sync::mpsc::UnboundedReceiver<Incoming>) {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| app.sessions.len() == 3).await;
    (app, transport, incoming)
}

#[tokio::test]
async fn the_arrow_keys_move_the_workspace_cursor() {
    let (mut app, mut transport, mut incoming) = ready().await;

    ctrl(&mut app, &mut transport, 'w');
    assert_eq!(app.view, View::Workspace, "^w should open the workspace list");
    let listed = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspaces.rows().len() == 2
    })
    .await;
    assert!(listed, "the workspace stream should fill the list");

    assert_eq!(app.workspace_row, 0);
    press(&mut app, &mut transport, KeyCode::Down);
    assert_eq!(app.workspace_row, 1, "Down must reach the workspace cursor");
    press(&mut app, &mut transport, KeyCode::Up);
    assert_eq!(app.workspace_row, 0, "Up must reach the workspace cursor");

    // Enter takes the highlighted one and leaves the surface.
    let expected = app.workspaces.rows()[0].path.clone();
    press(&mut app, &mut transport, KeyCode::Enter);
    assert_eq!(app.workspace.as_deref(), Some(expected.as_str()));
    assert_eq!(app.view, View::Conversation);

    transport.shutdown().await;
}

#[tokio::test]
async fn a_picker_opened_from_the_workspace_list_owns_the_keyboard() {
    // The regression this file exists for. `n` opens the picker over the workspace list,
    // but the view is still `Workspace`; when the view was checked before the modals, the
    // list kept eating every keystroke while the picker sat on screen — so the arrows
    // moved a cursor nobody could see and Enter chose the wrong thing.
    let (mut app, mut transport, mut incoming) = ready().await;

    ctrl(&mut app, &mut transport, 'w');
    drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspaces.rows().len() == 2
    })
    .await;

    press(&mut app, &mut transport, KeyCode::Char('n'));
    let opened = drive(&mut app, &mut transport, &mut incoming, |app| app.picker.is_some()).await;
    assert!(opened, "n should open the directory picker: {:?}", app.log);
    assert_eq!(app.view, View::Workspace, "the picker sits over the list");

    let row_before = app.workspace_row;
    let selected_before = app.picker.as_ref().unwrap().selected;
    press(&mut app, &mut transport, KeyCode::Down);
    assert_eq!(
        app.workspace_row, row_before,
        "the list behind the modal must not move"
    );
    assert_ne!(
        app.picker.as_ref().unwrap().selected,
        selected_before,
        "Down must reach the picker, not the list behind it"
    );

    transport.shutdown().await;
}

#[tokio::test]
async fn escape_closes_the_picker_without_leaving_the_workspace_list() {
    let (mut app, mut transport, mut incoming) = ready().await;
    ctrl(&mut app, &mut transport, 'w');
    drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspaces.rows().len() == 2
    })
    .await;
    press(&mut app, &mut transport, KeyCode::Char('n'));
    drive(&mut app, &mut transport, &mut incoming, |app| app.picker.is_some()).await;

    press(&mut app, &mut transport, KeyCode::Esc);
    assert!(app.picker.is_none(), "esc dismisses the picker");
    assert_eq!(
        app.view,
        View::Workspace,
        "one esc closes the modal, it does not also leave the surface"
    );
    transport.shutdown().await;
}

#[tokio::test]
async fn the_other_surfaces_still_route_after_the_reorder() {
    // Hoisting the modals changed the order every block is tested in, so the bindings
    // that were already working have to be shown still working.
    let (mut app, mut transport, _incoming) = ready().await;

    // The log pane: plain characters filter, esc clears then leaves.
    ctrl(&mut app, &mut transport, 'l');
    assert_eq!(app.view, View::Logs);
    press(&mut app, &mut transport, KeyCode::Char('z'));
    assert_eq!(app.log_filter, "z", "typing should reach the log filter");
    press(&mut app, &mut transport, KeyCode::Esc);
    assert!(app.log_filter.is_empty(), "esc clears the filter first");
    press(&mut app, &mut transport, KeyCode::Esc);
    assert_eq!(app.view, View::Conversation, "a second esc leaves");

    // Settings still owns its own arrows.
    ctrl(&mut app, &mut transport, 's');
    assert_eq!(app.view, View::Settings);
    press(&mut app, &mut transport, KeyCode::Esc);
    assert_eq!(app.view, View::Conversation);

    // And the composer still receives ordinary typing.
    press(&mut app, &mut transport, KeyCode::Char('h'));
    press(&mut app, &mut transport, KeyCode::Char('i'));
    assert!(!app.composer.is_empty(), "typing should reach the composer");

    transport.shutdown().await;
}

#[tokio::test]
async fn tab_accepts_a_candidate_before_it_moves_focus() {
    // Ordering regression: a generic Tab arm placed first makes completion unreachable.
    // This used to be asserted by grepping the source for the two arms, because the
    // routing lived in the binary and could not be exercised. Now it presses Tab.
    use dsh_tui::app::{Candidate, Pane};

    let (mut app, mut transport, _incoming) = ready().await;
    app.focus = Pane::Conversation;
    for ch in "/mo".chars() {
        app.composer.insert(ch);
    }
    app.candidates = vec![Candidate {
        label: "/model".into(),
        detail: "Choose the conversation model".into(),
        insert: "/model ".into(),
    }];
    app.candidate_index = 0;

    let focus_before = app.focus;
    press(&mut app, &mut transport, KeyCode::Tab);

    assert_eq!(app.focus, focus_before, "Tab must not move focus while a menu is open");
    assert!(app.candidates.is_empty(), "Tab should accept the candidate and close the menu");

    // And with no menu open, Tab is focus movement again.
    press(&mut app, &mut transport, KeyCode::Tab);
    assert_ne!(app.focus, focus_before, "Tab moves focus once no menu is open");

    transport.shutdown().await;
}

#[tokio::test]
async fn choosing_a_workspace_scopes_the_sidebar_and_clears_the_conversation() {
    // A workspace is a scope, so changing it has to change what is on screen. A transcript
    // from a session the new workspace does not contain is another project's conversation,
    // not "the last thing you were reading".
    let (mut app, mut transport, mut incoming) = ready().await;
    ctrl(&mut app, &mut transport, 'w');
    let ready = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspaces.rows().len() == 2 && app.followed_session().is_some()
    })
    .await;
    assert!(ready, "expected the workspace rows and a followed session");

    // Unscoped, every session the host has is listed.
    assert_eq!(app.visible_sessions().len(), app.sessions.len());
    assert!(!app.sessions_are_scoped());
    let followed_before = app.followed_session().map(str::to_string);
    assert!(followed_before.is_some(), "a session should be followed to start");

    // w-2 accounts for s-3 only, so the list narrows and the transcript must not survive.
    let rows = app.workspaces.rows();
    let other = rows.iter().find(|row| !row.session_ids.contains(
        &followed_before.clone().unwrap()
    )).expect("a workspace that does not contain the followed session");
    let path = other.path.clone();
    let expected = other.session_ids.clone();

    app.workspace_row = rows.iter().position(|row| row.path == path).unwrap();
    press(&mut app, &mut transport, KeyCode::Enter); // conversation view; harmless
    app.use_selected_workspace(&mut transport);

    assert_eq!(app.workspace.as_deref(), Some(path.as_str()));
    assert!(app.sessions_are_scoped(), "the sidebar should be scoped now");
    let visible: Vec<String> = app
        .visible_sessions()
        .iter()
        .map(|i| app.sessions[*i].id.clone())
        .collect();
    assert_eq!(visible, expected, "only the workspace's sessions are listed");
    assert_ne!(
        app.followed_session().map(str::to_string), followed_before,
        "the out-of-workspace transcript must not still be followed"
    );
    assert!(
        app.visible_sessions().contains(&app.selected_session),
        "the cursor must land inside the new workspace"
    );

    transport.shutdown().await;
}

#[tokio::test]
async fn an_unaccounted_workspace_does_not_blank_the_sidebar() {
    // A workspace chosen by path before its row arrives — or a host that does not account
    // sessions to workspaces at all — must show every session rather than none.
    let (mut app, mut transport, mut incoming) = ready().await;
    ctrl(&mut app, &mut transport, 'w');
    drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspaces.rows().len() == 2
    })
    .await;

    app.workspace = Some("/somewhere/the/host/never/mentioned".to_string());
    assert_eq!(
        app.visible_sessions().len(),
        app.sessions.len(),
        "an unknown workspace must not hide every session"
    );
    assert!(!app.sessions_are_scoped());
    transport.shutdown().await;
}

#[tokio::test]
async fn the_arrow_keys_skip_sessions_outside_the_workspace() {
    use dsh_tui::app::Pane;

    let (mut app, mut transport, mut incoming) = ready().await;
    ctrl(&mut app, &mut transport, 'w');
    drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspaces.rows().len() == 2
    })
    .await;

    // Scope to the workspace that holds more than one session, so there is somewhere to move.
    let rows = app.workspaces.rows();
    let multi = rows
        .iter()
        .max_by_key(|row| row.session_ids.len())
        .expect("a workspace");
    app.workspace_row = rows.iter().position(|r| r.path == multi.path).unwrap();
    app.use_selected_workspace(&mut transport);

    let visible = app.visible_sessions();
    // Down/Up only mean "move session" while the sidebar has focus.
    app.focus = Pane::Sidebar;
    for _ in 0..6 {
        press(&mut app, &mut transport, KeyCode::Down);
        assert!(
            visible.contains(&app.selected_session),
            "Down must never land on a session outside the workspace"
        );
    }
    for _ in 0..6 {
        press(&mut app, &mut transport, KeyCode::Up);
        assert!(
            visible.contains(&app.selected_session),
            "Up must never land on a session outside the workspace"
        );
    }
    transport.shutdown().await;
}

#[tokio::test]
async fn the_picker_pages_through_a_long_listing() {
    // The reported bug: the cursor reached the bottom of the window and there was no way
    // to go further — no paging, and the rows past the window were never drawn.
    let (mut app, mut transport, mut incoming) = ready().await;
    ctrl(&mut app, &mut transport, 'o');
    let opened = drive(&mut app, &mut transport, &mut incoming, |app| app.picker.is_some()).await;
    assert!(opened, "^o should open the picker");

    // The page size is measured from the frame, so a layout has to have happened.
    app.measure(24);
    assert!(app.picker_page > 1, "a page should be more than one row");

    let last = app.picker.as_ref().unwrap().cursor_len() - 1;
    for _ in 0..40 {
        press(&mut app, &mut transport, KeyCode::PageDown);
    }
    assert_eq!(
        app.picker.as_ref().unwrap().selected,
        last,
        "PageDown must reach the end and stop there"
    );
    press(&mut app, &mut transport, KeyCode::Home);
    assert_eq!(app.picker.as_ref().unwrap().selected, 0, "Home returns to the top");
    press(&mut app, &mut transport, KeyCode::End);
    assert_eq!(app.picker.as_ref().unwrap().selected, last, "End reaches the last row");
    for _ in 0..40 {
        press(&mut app, &mut transport, KeyCode::PageUp);
    }
    assert_eq!(app.picker.as_ref().unwrap().selected, 0, "PageUp stops at the top");

    transport.shutdown().await;
}

#[tokio::test]
async fn the_session_pane_navigates_and_opens_with_enter() {
    // With focus on the sidebar the arrows move through sessions and enter opens the
    // highlighted one to continue it — which means focus has to end up where typing goes.
    use dsh_tui::app::Pane;

    let (mut app, mut transport, mut incoming) = ready().await;
    drive(&mut app, &mut transport, &mut incoming, |app| {
        app.followed_session().is_some()
    })
    .await;

    // Tab reaches the sidebar rather than the pane being focusable only by assignment.
    while app.focus != Pane::Sidebar {
        press(&mut app, &mut transport, KeyCode::Tab);
    }

    let first = app.selected_session;
    press(&mut app, &mut transport, KeyCode::Down);
    assert_ne!(app.selected_session, first, "Down should move to the next session");
    let second = app.selected_session;
    press(&mut app, &mut transport, KeyCode::Up);
    assert_eq!(app.selected_session, first, "Up should come back");

    // Home and End reach the ends of the list.
    press(&mut app, &mut transport, KeyCode::End);
    assert_eq!(app.selected_session, *app.visible_sessions().last().unwrap());
    press(&mut app, &mut transport, KeyCode::Home);
    assert_eq!(app.selected_session, *app.visible_sessions().first().unwrap());

    // Enter opens the highlighted session: it is followed, and the keyboard moves to the
    // composer so the next keystroke continues the conversation.
    press(&mut app, &mut transport, KeyCode::Down);
    let wanted = app.sessions[app.selected_session].id.clone();
    press(&mut app, &mut transport, KeyCode::Enter);
    assert_eq!(app.focus, Pane::Conversation, "enter should hand over the keyboard");

    let opened = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.followed_session() == Some(wanted.as_str())
    })
    .await;
    assert!(opened, "the opened session should be followed: {:?}", app.log);
    assert_eq!(app.selected_session, second.max(app.selected_session));

    // And typing now reaches the composer rather than moving the cursor.
    press(&mut app, &mut transport, KeyCode::Char('h'));
    assert!(!app.composer.is_empty(), "typing should continue the session");

    transport.shutdown().await;
}

#[tokio::test]
async fn the_picker_climbs_back_out_to_the_parent() {
    // Descending into a workspace and then trying to get back out to pick a sibling.
    let (mut app, mut transport, mut incoming) = ready().await;
    ctrl(&mut app, &mut transport, 'o');
    drive(&mut app, &mut transport, &mut incoming, |app| app.picker.is_some()).await;

    let at = |app: &App| app.picker.as_ref().unwrap().listing.path.clone();
    let start = at(&app);
    assert_eq!(start, "/home/acp/projects");

    // Descend twice.
    press(&mut app, &mut transport, KeyCode::Down); // off the synthetic row
    press(&mut app, &mut transport, KeyCode::Right);
    let down_one = drive(&mut app, &mut transport, &mut incoming, |app| at(app) != start).await;
    assert!(down_one, "→ should descend: {:?}", app.log);
    let deep = at(&app);
    assert_eq!(deep, "/home/acp/projects/dsh-tui");

    press(&mut app, &mut transport, KeyCode::Down);
    press(&mut app, &mut transport, KeyCode::Right);
    let down_two = drive(&mut app, &mut transport, &mut incoming, |app| at(app) != deep).await;
    assert!(down_two, "→ should descend again: {:?}", app.log);
    assert_eq!(at(&app), "/home/acp/projects/dsh-tui/dsh-tui");

    // Now climb back out, one level per press, all the way to the root.
    for expected in [
        "/home/acp/projects/dsh-tui",
        "/home/acp/projects",
        "/home/acp",
        "/home",
        "/",
    ] {
        let before = at(&app);
        press(&mut app, &mut transport, KeyCode::Left);
        let up = drive(&mut app, &mut transport, &mut incoming, |app| at(app) != before).await;
        assert!(up, "← should reach {expected} from {before}: {:?}", app.log);
        assert_eq!(at(&app), expected, "← climbed to the wrong level");
    }

    // At the filesystem root there is no parent, and ← must be a no-op rather than a hang.
    press(&mut app, &mut transport, KeyCode::Left);
    assert_eq!(at(&app), "/");

    transport.shutdown().await;
}

#[tokio::test]
async fn a_refused_parent_says_why_instead_of_doing_nothing() {
    // The reported symptom: `←` appears not to work. Whatever the host's reason — a
    // permission, a vanished directory, a root it will not browse above — the dialog has
    // to say it, because silence is indistinguishable from a broken key.
    let (program, args) = stub();
    let (mut transport, mut incoming) = Transport::spawn_with_env(
        &program,
        &args,
        &[("DSH_TUI_STUB_PICKER_FLOOR", "/home/acp/projects")],
    )
    .expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| app.sessions.len() == 3).await;

    ctrl(&mut app, &mut transport, 'o');
    drive(&mut app, &mut transport, &mut incoming, |app| app.picker.is_some()).await;
    assert_eq!(app.picker.as_ref().unwrap().listing.path, "/home/acp/projects");

    // `/home/acp` is above the stub's floor, so the host refuses it.
    press(&mut app, &mut transport, KeyCode::Left);
    let told = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.picker_error.is_some()
    })
    .await;
    assert!(told, "a refused listing must be reported: {:?}", app.log);
    assert!(
        app.picker_error.as_deref().unwrap().contains("outside the browsable roots"),
        "the host's own reason should reach the dialog: {:?}", app.picker_error
    );
    // The dialog stays open on the level that still works, rather than emptying.
    assert_eq!(app.picker.as_ref().unwrap().listing.path, "/home/acp/projects");

    transport.shutdown().await;
}

#[tokio::test]
async fn at_the_top_of_the_tree_the_left_key_explains_itself() {
    let (mut app, mut transport, mut incoming) = ready().await;
    ctrl(&mut app, &mut transport, 'o');
    drive(&mut app, &mut transport, &mut incoming, |app| app.picker.is_some()).await;

    let at = |app: &App| app.picker.as_ref().unwrap().listing.path.clone();
    for _ in 0..3 {
        let before = at(&app);
        press(&mut app, &mut transport, KeyCode::Left);
        drive(&mut app, &mut transport, &mut incoming, |app| at(app) != before).await;
    }
    assert_eq!(at(&app), "/", "three levels up from ~/projects is the root");

    // There is no crumb above the root, so ← has nowhere to go and says so.
    press(&mut app, &mut transport, KeyCode::Left);
    assert!(
        app.picker_error.as_deref().is_some_and(|e| e.contains("top of what the host")),
        "expected an explanation, got {:?}", app.picker_error
    );
    transport.shutdown().await;
}

#[tokio::test]
async fn a_second_navigation_supersedes_one_still_in_flight() {
    // Pressing ← twice quickly used to drop the second key entirely, which looks exactly
    // like a directory that cannot be left.
    let (mut app, mut transport, mut incoming) = ready().await;
    ctrl(&mut app, &mut transport, 'o');
    drive(&mut app, &mut transport, &mut incoming, |app| app.picker.is_some()).await;

    // Two presses with no chance to settle in between.
    press(&mut app, &mut transport, KeyCode::Left);
    press(&mut app, &mut transport, KeyCode::Left);

    let settled = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.picker.as_ref().unwrap().listing.path == "/home"
    })
    .await;
    assert!(
        settled,
        "the second press must not be swallowed: at {:?}, log {:?}",
        app.picker.as_ref().unwrap().listing.path,
        app.log
    );
    transport.shutdown().await;
}

#[tokio::test]
async fn a_first_time_workspace_clears_the_screen_and_is_ready_to_type() {
    // The reported bug: registering a new workspace left the previous one's sessions in
    // the sidebar and its transcript in the conversation. A workspace with no sessions was
    // falling back to "show everything", so nothing looked like it had changed.
    let (mut app, mut transport, mut incoming) = ready().await;
    ctrl(&mut app, &mut transport, 'w');
    drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspaces.rows().len() == 2 && app.followed_session().is_some()
    })
    .await;
    assert!(!app.ledger.is_empty(), "a transcript should be on screen to start");

    // Register a directory the host has never seen: a brand new, empty workspace.
    press(&mut app, &mut transport, KeyCode::Char('n'));
    drive(&mut app, &mut transport, &mut incoming, |app| app.picker.is_some()).await;
    press(&mut app, &mut transport, KeyCode::Down);
    press(&mut app, &mut transport, KeyCode::Enter);
    // The registration reply and the row landing on the live feed are separate frames;
    // the scope is only final once the row has arrived.
    let registered = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspace.as_deref() == Some("/home/acp/projects/dsh-tui")
            && app.workspaces.rows().len() == 3
    })
    .await;
    assert!(registered, "the new workspace should be adopted: {:?}", app.log);

    // Nothing from the old workspace survives.
    assert!(app.workspace_is_empty(), "a new workspace has no sessions");
    assert!(app.visible_sessions().is_empty(), "the sidebar must not show the old ones");
    assert!(app.ledger.is_empty(), "the old transcript must be gone");
    assert_eq!(app.followed_session(), None, "and nothing is still being followed");
    // Critically: there is no active session, so a prompt cannot land in another project.
    assert_eq!(app.active_session_id(), None);

    transport.shutdown().await;
}

#[tokio::test]
async fn typing_in_a_fresh_workspace_starts_the_session_itself() {
    // "Show the chat box so we can start": the composer is live before any session
    // exists, and sending creates one rather than reporting that none is selected.
    let (mut app, mut transport, mut incoming) = ready().await;
    ctrl(&mut app, &mut transport, 'w');
    drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspaces.rows().len() == 2
    })
    .await;
    press(&mut app, &mut transport, KeyCode::Char('n'));
    drive(&mut app, &mut transport, &mut incoming, |app| app.picker.is_some()).await;
    press(&mut app, &mut transport, KeyCode::Down);
    press(&mut app, &mut transport, KeyCode::Enter);
    let fresh = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspaces.rows().len() == 3 && app.workspace_is_empty()
    })
    .await;
    assert!(fresh, "expected an empty new workspace: {:?}", app.log);

    let before = app.sessions.len();
    for ch in "hello".chars() {
        press(&mut app, &mut transport, KeyCode::Char(ch));
    }
    press(&mut app, &mut transport, KeyCode::Enter);

    let started = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.sessions.len() > before && app.active_session_id().is_some()
    })
    .await;
    assert!(started, "typing should create the session: {:?}", app.log);
    // The prompt is not dropped on the way: it is sent once the session exists.
    assert!(
        app.log.iter().any(|r| r.event == "prompt.deferred"),
        "the deferral should be recorded: {:?}", app.log
    );
    assert!(app.composer.is_empty(), "the composer is cleared once it is sent");
    transport.shutdown().await;
}

#[tokio::test]
async fn returning_to_a_workspace_shows_its_own_transcript_again() {
    // The other half: coming back should restore that workspace's session, not a blank.
    let (mut app, mut transport, mut incoming) = ready().await;
    ctrl(&mut app, &mut transport, 'w');
    drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspaces.rows().len() == 2
    })
    .await;

    // Take the workspace that owns a session, and confirm its transcript is bound.
    let rows = app.workspaces.rows();
    let occupied = rows.iter().find(|r| !r.session_ids.is_empty()).expect("one with a session");
    let wanted = occupied.session_ids[0].clone();
    app.workspace_row = rows.iter().position(|r| r.path == occupied.path).unwrap();
    press(&mut app, &mut transport, KeyCode::Enter);
    let bound = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.followed_session() == Some(wanted.as_str())
    })
    .await;
    assert!(bound, "returning should follow the workspace's session: {:?}", app.log);
    assert!(!app.workspace_is_empty());
    assert_eq!(app.active_session_id(), Some(wanted.as_str()));
    transport.shutdown().await;
}

/// Open a fresh, empty workspace via `^w` + `n`, the way the surface is actually used.
async fn fresh_workspace(
    app: &mut App,
    transport: &mut Transport,
    incoming: &mut tokio::sync::mpsc::UnboundedReceiver<Incoming>,
) {
    ctrl(app, transport, 'w');
    drive(app, transport, incoming, |app| app.workspaces.rows().len() == 2).await;
    press(app, transport, KeyCode::Char('n'));
    drive(app, transport, incoming, |app| app.picker.is_some()).await;
    press(app, transport, KeyCode::Down);
    press(app, transport, KeyCode::Enter);
    let ready = drive(app, transport, incoming, |app| {
        app.workspaces.rows().len() == 3 && app.workspace_is_empty()
    })
    .await;
    assert!(ready, "expected an empty new workspace: {:?}", app.log);
}

#[tokio::test]
async fn a_rejected_prompt_says_so_and_gives_the_text_back() {
    // The reported symptom: typed hello, nothing happened, no acknowledgement. The
    // composer empties on submit, so a rejection with no feedback loses the message and
    // looks exactly like a key that did nothing.
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn_with_env(&program, &args, &[("DSH_TUI_STUB_REJECT_PROMPT", "1")])
            .expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| app.sessions.len() == 3).await;
    fresh_workspace(&mut app, &mut transport, &mut incoming).await;

    for ch in "hello".chars() {
        press(&mut app, &mut transport, KeyCode::Char(ch));
    }
    press(&mut app, &mut transport, KeyCode::Enter);

    let told = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.prompt_error.is_some()
    })
    .await;
    assert!(told, "a refused prompt must be reported: {:?}", app.log);
    assert!(
        app.prompt_error.as_deref().unwrap().contains("not accepting messages"),
        "the host's reason should reach the screen: {:?}", app.prompt_error
    );
    transport.shutdown().await;
}

#[tokio::test]
async fn a_prompt_that_cannot_get_a_session_keeps_what_was_typed() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn_with_env(&program, &args, &[("DSH_TUI_STUB_REJECT_CREATE", "1")])
            .expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| app.sessions.len() == 3).await;
    fresh_workspace(&mut app, &mut transport, &mut incoming).await;

    for ch in "hello".chars() {
        press(&mut app, &mut transport, KeyCode::Char(ch));
    }
    press(&mut app, &mut transport, KeyCode::Enter);
    assert!(app.composer.is_empty(), "submit empties the composer");

    let told = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.prompt_error.is_some()
    })
    .await;
    assert!(told, "a failed creation must be reported: {:?}", app.log);
    assert!(app.prompt_error.as_deref().unwrap().contains("could not start a session"));
    // What was typed comes back rather than vanishing.
    assert!(!app.composer.is_empty(), "the text should be restored to the composer");
    transport.shutdown().await;
}

#[tokio::test]
async fn a_deferred_prompt_reaches_the_session_that_was_made_for_it() {
    let (mut app, mut transport, mut incoming) = ready().await;
    fresh_workspace(&mut app, &mut transport, &mut incoming).await;

    for ch in "hello".chars() {
        press(&mut app, &mut transport, KeyCode::Char(ch));
    }
    press(&mut app, &mut transport, KeyCode::Enter);

    // The receipt lands on the create reply; the attach reaches the workspace roster on
    // a later frame, so both have to settle before the association can be checked.
    let accepted = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.log.iter().any(|r| r.event == "prompt.accepted")
            && app
                .workspaces
                .rows()
                .iter()
                .any(|row| Some(row.path.as_str()) == app.workspace.as_deref()
                    && !row.session_ids.is_empty())
    })
    .await;
    assert!(accepted, "the host should receipt the prompt: {:?}", app.log);
    assert!(app.prompt_error.is_none(), "nothing should be reported as failed");

    // It went to the session created for it, not to one from another workspace.
    let sent = app
        .log
        .iter()
        .find(|r| r.event == "prompt.sent")
        .expect("the send should be recorded");
    let session = sent.fields.get("sessionId").and_then(|v| v.as_str()).unwrap();
    assert!(
        app.workspaces
            .rows()
            .iter()
            .find(|row| Some(row.path.as_str()) == app.workspace.as_deref())
            .is_some_and(|row| row.session_ids.iter().any(|id| id == session)),
        "the prompt's session must belong to the current workspace"
    );
    transport.shutdown().await;
}

#[tokio::test]
async fn a_host_that_never_accounts_a_session_still_gets_one_session() {
    // Straight from a real log: three prompts, three `prompt.deferred`, three sessions
    // created in the same workspace, and the workspace still reporting itself empty. The
    // host created each session with the right `cwd` but never added it to the
    // workspace's roster, so the client saw nothing claiming the workspace.
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn_with_env(&program, &args, &[("DSH_TUI_STUB_NO_ACCOUNTING", "1")])
            .expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| app.sessions.len() == 3).await;
    fresh_workspace(&mut app, &mut transport, &mut incoming).await;

    let before = app.sessions.len();
    for ch in "hello".chars() {
        press(&mut app, &mut transport, KeyCode::Char(ch));
    }
    press(&mut app, &mut transport, KeyCode::Enter);
    // The receipt arrives on the create reply, ahead of the list refresh that makes the
    // session visible; both have to land before the scope can be judged.
    let first = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.log.iter().any(|r| r.event == "prompt.accepted") && app.sessions.len() > before
    })
    .await;
    assert!(first, "the first prompt should be accepted: {:?}", app.log);
    assert_eq!(app.sessions.len(), before + 1, "exactly one session was created");

    // The session names this directory as its cwd, so the workspace claims it even though
    // the host's roster never mentioned it.
    assert!(!app.workspace_is_empty(), "the workspace must not still look empty");
    assert!(app.active_session_id().is_some(), "and it has an active session");

    // The second message reuses it rather than starting another.
    for ch in "again".chars() {
        press(&mut app, &mut transport, KeyCode::Char(ch));
    }
    press(&mut app, &mut transport, KeyCode::Enter);
    let second = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.log.iter().filter(|r| r.event == "prompt.accepted").count() == 2
    })
    .await;
    assert!(second, "the second prompt should also be accepted: {:?}", app.log);
    assert_eq!(
        app.sessions.len(),
        before + 1,
        "a second message must not create a second session"
    );
    assert_eq!(
        app.log.iter().filter(|r| r.event == "prompt.deferred").count(),
        1,
        "only the first message should have had to defer"
    );
    transport.shutdown().await;
}

#[tokio::test]
async fn two_quick_messages_before_a_session_exists_share_one() {
    // Enter twice before the creation settles used to start a session per press.
    let (mut app, mut transport, mut incoming) = ready().await;
    fresh_workspace(&mut app, &mut transport, &mut incoming).await;
    let before = app.sessions.len();

    for ch in "one".chars() {
        press(&mut app, &mut transport, KeyCode::Char(ch));
    }
    press(&mut app, &mut transport, KeyCode::Enter);
    // No `drive` in between: the creation is still in flight.
    for ch in "two".chars() {
        press(&mut app, &mut transport, KeyCode::Char(ch));
    }
    press(&mut app, &mut transport, KeyCode::Enter);

    let done = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.log.iter().any(|r| r.event == "prompt.accepted") && app.sessions.len() > before
    })
    .await;
    assert!(done, "the queued prompt should still go out: {:?}", app.log);
    assert_eq!(app.sessions.len(), before + 1, "one session, not two");
    assert!(
        app.log.iter().any(|r| r.event == "prompt.queued"),
        "the second press should have queued: {:?}", app.log
    );
    transport.shutdown().await;
}

#[tokio::test]
async fn session_creation_names_one_locator_and_it_is_the_workspace() {
    // The host rejects a request carrying both `workspaceId` and `cwd`, and only the id
    // makes it attach the session to the workspace. Sending the path alone created the
    // session in the right directory and joined it to nothing.
    let (mut app, mut transport, mut incoming) = ready().await;
    fresh_workspace(&mut app, &mut transport, &mut incoming).await;

    for ch in "hello".chars() {
        press(&mut app, &mut transport, KeyCode::Char(ch));
    }
    press(&mut app, &mut transport, KeyCode::Enter);
    let accepted = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.log.iter().any(|r| r.event == "prompt.accepted")
    })
    .await;
    assert!(accepted, "the prompt should be accepted: {:?}", app.log);
    assert!(app.prompt_error.is_none(), "got {:?}", app.prompt_error);

    // Exactly one locator went out, and it was the workspace id.
    let request = app
        .log
        .iter()
        .find(|r| r.event == "call.out" && r.message.contains("session.create"))
        .and_then(|r| r.fields.get("args").cloned())
        .expect("the create request should be logged");
    let request = request.get("request").expect("a request object");
    assert!(
        request.get("workspaceId").is_some(),
        "the id is what makes the host attach the session: {request}"
    );
    assert!(
        request.get("cwd").is_none(),
        "sending both is rejected by the host: {request}"
    );
    transport.shutdown().await;
}

#[tokio::test]
async fn a_workspace_the_host_forgot_falls_back_to_the_directory() {
    // A stale id would otherwise leave every creation failing with no way out.
    let (mut app, mut transport, mut incoming) = ready().await;
    ctrl(&mut app, &mut transport, 'w');
    drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspaces.rows().len() == 2
    })
    .await;
    press(&mut app, &mut transport, KeyCode::Enter); // adopt the first workspace

    // Forge an id the host has never issued.
    app.set_workspace_id_for_test(Some("w-gone".to_string()));
    app.new_session(&mut transport);

    let recovered = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.log.iter().any(|r| r.event == "session.create.retry")
    })
    .await;
    assert!(recovered, "a forgotten workspace should be retried by path: {:?}", app.log);
    let made = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.sessions.len() == 4
    })
    .await;
    assert!(made, "the retry should still produce a session: {:?}", app.log);
    transport.shutdown().await;
}

#[tokio::test]
async fn scrolling_the_session_list_browses_and_only_enter_opens() {
    // Following on every arrow tore down and rebuilt a transcript stream per keypress,
    // and left `enter` with nothing to do.
    use dsh_tui::app::Pane;

    let (mut app, mut transport, mut incoming) = ready().await;
    drive(&mut app, &mut transport, &mut incoming, |app| {
        app.followed_session().is_some()
    })
    .await;
    while app.focus != Pane::Sidebar {
        press(&mut app, &mut transport, KeyCode::Tab);
    }

    let open_before = app.followed_session().map(str::to_string);
    press(&mut app, &mut transport, KeyCode::Down);
    assert_ne!(
        app.sessions[app.selected_session].id,
        open_before.clone().unwrap(),
        "the cursor should have moved off the open session"
    );
    assert_eq!(
        app.followed_session().map(str::to_string),
        open_before,
        "browsing must not switch the transcript"
    );

    // Enter is what opens it.
    let wanted = app.sessions[app.selected_session].id.clone();
    press(&mut app, &mut transport, KeyCode::Enter);
    let opened = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.followed_session() == Some(wanted.as_str())
    })
    .await;
    assert!(opened, "enter should open the highlighted session: {:?}", app.log);
    transport.shutdown().await;
}

#[tokio::test]
async fn opening_a_session_moves_the_workspace_with_it() {
    // The status bar and the sidebar header both name the workspace, so opening a session
    // from another one has to carry them along or they describe somewhere it does not live.
    use dsh_tui::app::Pane;

    let (mut app, mut transport, mut incoming) = ready().await;
    ctrl(&mut app, &mut transport, 'w');
    drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspaces.rows().len() == 2
    })
    .await;
    // Adopt the first workspace, so there is one to move away from.
    press(&mut app, &mut transport, KeyCode::Enter);
    let first = app.workspace.clone().expect("a workspace");

    // Find a session belonging to a different workspace.
    let rows = app.workspaces.rows();
    let other = rows
        .iter()
        .find(|row| row.path != first && !row.session_ids.is_empty())
        .expect("another workspace with a session");
    let target = other.session_ids[0].clone();
    let other_path = other.path.clone();

    app.selected_session = app
        .sessions
        .iter()
        .position(|s| s.id == target)
        .expect("the session should be listed");
    app.focus = Pane::Sidebar;
    press(&mut app, &mut transport, KeyCode::Enter);

    assert_eq!(
        app.workspace.as_deref(),
        Some(other_path.as_str()),
        "the workspace should have followed the session"
    );
    assert!(
        app.log.iter().any(|r| r.event == "workspace.followed"),
        "the move should be recorded: {:?}", app.log
    );
    // And the header now names that session as the open one.
    let open = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.followed_session() == Some(target.as_str())
    })
    .await;
    assert!(open, "the session should be followed: {:?}", app.log);
    assert_eq!(app.open_session().map(|s| s.id.clone()), Some(target));
    transport.shutdown().await;
}
