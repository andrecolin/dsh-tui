//! Cross-language protocol tests.
//!
//! These drive the real Rust transport and reducer against `bridge/dev-stub.mjs`, so a
//! pass proves the TypeScript and Rust ends agree on the wire — the failure mode a
//! hand-written protocol invites. They prove nothing about harness behavior.

use std::time::Duration;

use dsh_tui::app::{App, AskReply, Connection};
use dsh_tui::theme::Theme;
use dsh_tui::transport::Transport;

fn stub() -> (String, Vec<String>) {
    let path = format!("{}/../../bridge/dev-stub.mjs", env!("CARGO_MANIFEST_DIR"));
    ("node".to_string(), vec![path])
}

/// Drive the app until `done` holds, or time out.
async fn drive(
    app: &mut App,
    transport: &mut Transport,
    incoming: &mut tokio::sync::mpsc::UnboundedReceiver<dsh_tui::transport::Incoming>,
    done: impl Fn(&App) -> bool,
) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if done(app) {
            return true;
        }
        match tokio::time::timeout_at(deadline, incoming.recv()).await {
            Ok(Some(msg)) => app.on_incoming(msg, transport),
            Ok(None) => break,
            Err(_) => break,
        }
    }
    done(app)
}

#[tokio::test]
async fn handshake_completes_and_loads_sessions() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());

    let ok = drive(&mut app, &mut transport, &mut incoming, |app| {
        matches!(app.connection, Connection::Ready(_)) && !app.sessions.is_empty()
    })
    .await;

    assert!(ok, "expected a completed handshake and a session list");
    assert_eq!(app.sessions.len(), 3);
    assert_eq!(app.sessions[0].title, "Wire the trajectory pane");
    assert!(app.sessions[0].running);
    transport.shutdown().await;
}

#[tokio::test]
async fn a_waterfall_arrives_and_can_be_allowed() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn_with_env(&program, &args, &[("DSH_TUI_STUB_ASK", "1")])
            .expect("stub should spawn");
    let mut app = App::new(Theme::default());

    let arrived = drive(&mut app, &mut transport, &mut incoming, |app| !app.asks.is_empty()).await;
    assert!(arrived, "expected the waterfall to reach the front end");

    let ask = app.asks.front().expect("one pending ask");
    assert!(ask.is_renderable());
    assert_eq!(ask.summary(), "Run `rm -rf build/`");

    app.answer_ask(AskReply::Allow, &mut transport);
    let echoed = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.log.iter().any(|line| line.contains("resolved via answer"))
    })
    .await;
    assert!(echoed, "the stub should confirm it received an answer");
    transport.shutdown().await;
}

#[tokio::test]
async fn an_unrenderable_waterfall_is_delegated_not_decided() {
    // The important guarantee: a request shape this build cannot show must go back to the
    // host's own listener rather than being answered on the user's behalf.
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn_with_env(&program, &args, &[("DSH_TUI_STUB_ASK", "opaque")])
            .expect("stub should spawn");
    let mut app = App::new(Theme::default());

    let arrived = drive(&mut app, &mut transport, &mut incoming, |app| !app.asks.is_empty()).await;
    assert!(arrived, "expected the opaque waterfall to arrive");
    assert!(!app.asks.front().unwrap().is_renderable());

    app.answer_ask(AskReply::Delegate, &mut transport);
    let delegated = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.log.iter().any(|line| line.contains("resolved via next"))
    })
    .await;
    assert!(delegated, "the stub should confirm delegation, not an answer");
    transport.shutdown().await;
}

#[tokio::test]
async fn a_dead_runtime_surfaces_as_a_disconnect() {
    let (mut transport, mut incoming) =
        Transport::spawn("node", &["-e".to_string(), "process.exit(3)".to_string()])
            .expect("node should spawn");
    let mut app = App::new(Theme::default());

    let noticed = drive(&mut app, &mut transport, &mut incoming, |app| {
        matches!(app.connection, Connection::Failed(_))
    })
    .await;

    assert!(noticed, "a runtime that dies must not leave the UI reading 'connecting'");
    transport.shutdown().await;
}

#[tokio::test]
async fn the_conversation_populates_from_the_follow_stream() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());

    let ok = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.ledger.rows().len() >= 4
    })
    .await;
    assert!(ok, "expected the follow stream to fill the ledger");

    let rows = app.ledger.rows();
    assert_eq!(rows[0].kind, dsh_tui::session::RowKind::User);
    assert!(rows[0].text.contains("dropping the last token"));
    // Three fragments packed into one row must read as one sentence.
    assert_eq!(rows[1].text, "Checking the tokenizer bounds.");
    assert_eq!(rows[2].kind, dsh_tui::session::RowKind::ToolCall);
    // The card leads with the argument that says what the call does.
    assert_eq!(rows[2].text, "read_file  src/lex.rs");
    assert_eq!(rows[2].glyph, "▤");
    transport.shutdown().await;
}

#[tokio::test]
async fn streamed_appends_extend_the_same_assistant_row() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());

    // The stub appends a second answer fragment about a second after the baseline.
    let ok = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.ledger
            .rows()
            .iter()
            .any(|row| row.text.contains("off by one"))
    })
    .await;
    assert!(ok, "expected a live append to reach the ledger");
    // A gap or overlap would have been logged instead of applied.
    assert!(!app.log.iter().any(|line| line.contains("gap")), "unexpected gap: {:?}", app.log);
    transport.shutdown().await;
}

#[tokio::test]
async fn the_live_conversation_renders() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    let ready = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.ledger.rows().len() >= 4
    })
    .await;
    assert!(ready);

    let mut terminal = Terminal::new(TestBackend::new(96, 22)).expect("test backend");
    terminal
        .draw(|frame| dsh_tui::ui::render(frame, &app))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    let text: String = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    println!("{text}");

    // The pane shows the tail of a transcript longer than its height, so the opening
    // message is scrolled out; the ledger still holds it.
    assert!(app.ledger.rows()[0].text.contains("dropping the last token"));
    let visible_rows = text.lines().count();
    assert!(visible_rows > 0);
    // The newest rows are the ones on screen.
    assert!(text.contains("write") || text.contains("edit") || text.contains("src/parse.rs"));
    transport.shutdown().await;
}

#[tokio::test]
async fn typing_a_slash_opens_the_command_menu_from_the_host() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;

    app.composer.insert('/');
    app.refresh_candidates(&mut transport);
    // Two `/` sources answer independently, so wait for the one being asserted rather
    // than for the menu merely becoming non-empty.
    let listed = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.candidates.iter().any(|c| c.label == "/model")
    })
    .await;
    assert!(listed, "expected commands.list to answer");

    // Narrowing the query filters without another round trip.
    app.composer.insert('e');
    app.composer.insert('x');
    assert_eq!(app.visible_candidates().len(), 1);
    assert_eq!(app.visible_candidates()[0].label, "/export");

    app.pick_candidate(&mut transport);
    assert_eq!(app.composer.text(), "/export");
    transport.shutdown().await;
}

#[tokio::test]
async fn an_at_reference_queries_the_file_source() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;

    for ch in "look at @parse".chars() {
        app.composer.insert(ch);
    }
    app.refresh_candidates(&mut transport);
    let listed = drive(&mut app, &mut transport, &mut incoming, |app| !app.candidates.is_empty()).await;
    assert!(listed, "expected fileReferences.list to answer");

    app.pick_candidate(&mut transport);
    assert_eq!(app.composer.text(), "look at @src/parse.rs");
    transport.shutdown().await;
}

#[tokio::test]
async fn the_settings_form_builds_from_the_hosts_schema() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;

    app.open_settings(&mut transport);
    let loaded = drive(&mut app, &mut transport, &mut incoming, |app| app.settings.is_some()).await;
    assert!(loaded, "expected settings.describe to answer");

    let fields = app.settings_fields();
    let labels: Vec<_> = fields.iter().map(|f| f.label.as_str()).collect();
    assert_eq!(labels, vec!["baseUrl", "timeout", "stream", "mode", "apiKey"]);

    // The union of constants is a picker; the secret never shows its value.
    assert_eq!(
        fields[3].editor,
        dsh_tui::settings::Editor::Select(vec!["native".into(), "ptc".into()])
    );
    assert_eq!(fields[4].display_value(), "••••••••");
    // baseUrl is in the user layer, timeout is not.
    assert!(fields[0].overridden);
    assert!(!fields[1].overridden);
    transport.shutdown().await;
}

#[tokio::test]
async fn a_settings_write_carries_the_revision_and_is_reread() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;
    app.open_settings(&mut transport);
    drive(&mut app, &mut transport, &mut incoming, |app| app.settings.is_some()).await;

    let before = app.settings_namespace().unwrap().revision;
    app.settings_field = 1; // timeout
    app.write_setting("45", &mut transport);
    let applied = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.settings_namespace().map(|v| v.revision) != Some(before)
    })
    .await;
    assert!(applied, "expected the write to land and the document to be re-read");
    assert!(app.settings_error.is_none());
    transport.shutdown().await;
}

#[tokio::test]
async fn an_out_of_range_value_never_reaches_the_host() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;
    app.open_settings(&mut transport);
    drive(&mut app, &mut transport, &mut incoming, |app| app.settings.is_some()).await;

    let before = app.settings_namespace().unwrap().revision;
    app.settings_field = 1; // timeout, min 1 max 600
    app.write_setting("9999", &mut transport);
    assert!(app.settings_error.as_deref().unwrap().contains("at most 600"));
    // Rejected locally, so the revision never moved.
    assert_eq!(app.settings_namespace().unwrap().revision, before);
    transport.shutdown().await;
}

#[tokio::test]
async fn a_secret_is_not_written_through_the_settings_document() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;
    app.open_settings(&mut transport);
    drive(&mut app, &mut transport, &mut incoming, |app| app.settings.is_some()).await;

    app.settings_field = 4; // apiKey, a secret slot
    app.write_setting("sk-should-not-travel", &mut transport);
    // Secrets go to the credentials namespace; the settings document only records that a
    // slot is filled, so this must be refused before it leaves.
    assert!(app.settings_error.as_deref().unwrap().contains("credentials"));
    transport.shutdown().await;
}

#[tokio::test]
async fn the_workspace_browser_fills_from_its_stream() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;

    app.open_workspace(&mut transport);
    let filled = drive(&mut app, &mut transport, &mut incoming, |app| {
        !app.workspaces.is_empty()
    })
    .await;
    assert!(filled, "expected the workspace baseline");

    let rows = app.workspaces.rows();
    assert_eq!(rows.len(), 2);
    // s-2 is archived, so it must not appear under its workspace.
    assert_eq!(rows[0].session_ids, vec!["s-1"]);
    // The second workspace has no title and falls back to its path.
    assert_eq!(rows[1].title, "/home/acp/deepseek-harness");
    transport.shutdown().await;
}

#[tokio::test]
async fn the_models_page_joins_providers_settings_and_credentials() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;

    app.open_models(&mut transport);
    let joined = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.model_rows().len() == 3 && !app.credentials.is_empty()
    })
    .await;
    assert!(joined, "expected the provider directory and credentials to arrive");

    let rows = app.model_rows();

    // Registered, whole-section profile, credential in the environment.
    assert_eq!(rows[0].display_name, "DeepSeek");
    assert_eq!(rows[0].key_ref, "DEEPSEEK_OFFICIAL_API_KEY");
    assert!(rows[0].ready());

    // Registered and configured, but no credential for its derived reference.
    assert_eq!(rows[1].key_ref, "ANTHROPIC_API_KEY");
    assert!(!rows[1].ready());

    // The gateway names its own reference, which is unset, and the adapter never
    // registered the route.
    assert_eq!(rows[2].key_ref, "MY_GATEWAY_TOKEN");
    assert!(rows[2].key_ref_named);
    assert_eq!(rows[2].status(), "configured, adapter not registered");
    // Its profile is in the user layer with nothing beneath it.
    assert!(rows[2].removable);
    transport.shutdown().await;
}

#[tokio::test]
async fn a_credential_failure_still_leaves_the_models_page_usable() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn_with_env(&program, &args, &[("DSH_TUI_STUB_CRED_FAIL", "1")])
            .expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;

    app.open_models(&mut transport);
    let rendered = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.model_rows().len() == 3 && app.credential_error.is_some()
    })
    .await;
    // Credential state is an enrichment: the rows must still be there.
    assert!(rendered, "rows must survive a credential lookup failure");
    assert!(app.model_rows().iter().all(|row| row.credential.is_none()));
    transport.shutdown().await;
}

#[tokio::test]
async fn the_inventory_separates_disabled_plugins_from_broken_ones() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;

    app.open_plugins(&mut transport);
    let listed = drive(&mut app, &mut transport, &mut incoming, |app| {
        !app.inventory.is_empty()
    })
    .await;
    assert!(listed, "expected pluginInventory.list to answer");

    let summary = app.inventory.summary();
    assert_eq!(summary.total, 6);
    assert_eq!(summary.active, 2);
    assert_eq!(summary.disabled, 1);
    // Failed plus enabled-with-no-fiber; the disabled one is expected, not a problem, and
    // the unrecognized phase is an upstream addition rather than a fault.
    assert_eq!(summary.problems, 2);

    // Search narrows without another round trip.
    app.search_inventory(dsh_tui::app::SearchEdit::Push('t'));
    app.search_inventory(dsh_tui::app::SearchEdit::Push('o'));
    app.search_inventory(dsh_tui::app::SearchEdit::Push('o'));
    app.search_inventory(dsh_tui::app::SearchEdit::Push('l'));
    assert_eq!(app.inventory.rows().len(), 2);
    transport.shutdown().await;
}

#[tokio::test]
async fn narrowing_the_search_keeps_the_cursor_inside_the_list() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;
    app.open_plugins(&mut transport);
    drive(&mut app, &mut transport, &mut incoming, |app| !app.inventory.is_empty()).await;

    for _ in 0..5 {
        app.select_next_inventory_row();
    }
    assert_eq!(app.inventory_row, 5);
    // Filtering down to one row must not leave the cursor pointing past the end.
    for ch in "goal".chars() {
        app.search_inventory(dsh_tui::app::SearchEdit::Push(ch));
    }
    assert_eq!(app.inventory.rows().len(), 1);
    assert_eq!(app.inventory_row, 0);
    transport.shutdown().await;
}

#[tokio::test]
async fn the_stored_theme_preference_reaches_this_terminal() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;

    app.open_general(&mut transport);
    let adopted = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.theme_preference == dsh_tui::theme::Preference::Light
    })
    .await;
    // The stub stores `light`; a preference set in the browser must reach the terminal.
    assert!(adopted, "expected the ui-theme preference to be adopted");
    assert_eq!(app.theme.mode, dsh_tui::theme::Mode::Light);
    transport.shutdown().await;
}

#[tokio::test]
async fn cycling_the_theme_applies_locally_then_persists() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;
    app.open_general(&mut transport);
    drive(&mut app, &mut transport, &mut incoming, |app| {
        app.theme_preference == dsh_tui::theme::Preference::Light
    })
    .await;

    app.cycle_theme(&mut transport);
    // Applied before any round trip: the redraw must not wait on the host.
    assert_eq!(app.theme_preference, dsh_tui::theme::Preference::Dark);
    assert_eq!(app.theme.mode, dsh_tui::theme::Mode::Dark);

    let persisted = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.settings
            .as_ref()
            .and_then(|d| d.namespaces.iter().find(|ns| ns.ns == "ui-theme"))
            .and_then(|ns| ns.value.get("preference"))
            .and_then(|v| v.as_str())
            == Some("dark")
    })
    .await;
    assert!(persisted, "expected the preference to be written and re-read");
    assert!(app.settings_error.is_none());
    transport.shutdown().await;
}

#[tokio::test]
async fn a_malformed_tool_call_and_its_failure_both_surface() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    let ready = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.ledger.rows().len() >= 6
    })
    .await;
    assert!(ready, "expected the full transcript");

    let rows = app.ledger.rows();
    // The stub's second call carries truncated JSON, exactly as a model can emit it.
    let malformed = rows
        .iter()
        .find(|row| row.text.contains("malformed arguments"))
        .expect("the malformed call should still render");
    assert_eq!(malformed.glyph, "$");

    // Its result reports an internal error while isError is false.
    let failure = rows.iter().find(|row| row.failed).expect("a failed row");
    assert!(failure.text.contains("ParseError"));
    assert!(failure.text.contains("bad-arguments"));
    transport.shutdown().await;
}

#[tokio::test]
async fn the_trajectory_times_turns_and_tools_from_the_log() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| {
        app.ledger.rows().len() >= 6
    })
    .await;

    let turns = dsh_tui::trajectory::turns(&app.ledger);
    assert_eq!(turns.len(), 2);
    // 3400 - 1000, read from the events' own timestamps.
    assert_eq!(turns[0].millis, Some(2_400));
    // read_file, the malformed bash call, and the run_code program.
    assert_eq!(turns[0].tools.len(), 3);
    assert_eq!(turns[0].tools[0].name, "read_file");
    assert_eq!(turns[0].tools[0].millis, Some(1_250));
    assert!(turns[0].tools[1].failed);
    assert_eq!(turns[0].failures(), 1);
    // The second turn is still open.
    assert!(turns[1].is_open());
    transport.shutdown().await;
}

#[tokio::test]
async fn the_chips_read_live_projections_and_jobs() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    let ready = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.goal().is_some() && !app.sessions.is_empty()
    })
    .await;
    assert!(ready, "expected the control baseline");

    assert_eq!(app.plan_chip(), dsh_tui::chips::PlanChip::On);

    let goal = app.goal().expect("goal");
    assert_eq!(goal.summary(), "Ship the TUI · active · 3/20");
    // Active but disarmed: this process will not continue it on its own.
    assert_eq!(goal.will_continue(), Some(false));

    // Only running and stopping jobs count as live.
    assert_eq!(app.live_jobs(), 1);
    transport.shutdown().await;
}

#[tokio::test]
async fn the_model_picker_lists_choices_and_explains_absences() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;

    app.open_model_picker(&mut transport);
    let listed = drive(&mut app, &mut transport, &mut incoming, |app| {
        !app.catalog.choices.is_empty()
    })
    .await;
    assert!(listed, "expected the model catalog");

    // One provider failed and one listed nothing; neither may empty the picker.
    assert_eq!(app.model_choices().len(), 2);
    assert_eq!(app.catalog.failures.len(), 1);
    assert_eq!(app.catalog.empty_providers, vec!["empty-gw"]);

    app.search_models(dsh_tui::app::SearchEdit::Push('p'));
    app.search_models(dsh_tui::app::SearchEdit::Push('r'));
    app.search_models(dsh_tui::app::SearchEdit::Push('o'));
    assert_eq!(app.model_choices().len(), 1);

    app.pick_model(&mut transport);
    assert!(app.model_picker.is_none());
    transport.shutdown().await;
}

#[tokio::test]
async fn a_session_without_plan_mode_shows_no_plan_chip() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;

    // The stub projects plan only for s-1; selecting another session leaves the key absent.
    app.selected_session = 1;
    // Absence is capability absence, not "off": the chip takes no seat at all.
    assert_eq!(app.plan_chip(), dsh_tui::chips::PlanChip::Unavailable);
    assert!(!app.plan_chip().is_visible());
    transport.shutdown().await;
}

#[tokio::test]
async fn skills_and_commands_share_the_slash_menu() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;

    app.composer.insert('/');
    app.refresh_candidates(&mut transport);
    let merged = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.candidates.iter().any(|c| c.label == "/model")
            && app.candidates.iter().any(|c| c.label == "/code-review")
    })
    .await;
    // Both registries are `/` sources; a reader does not care which served an entry.
    assert!(merged, "expected commands and skills in one menu: {:?}", app.candidates);

    // `whenToUse` is routing guidance worth showing beside the description.
    let simplify = app
        .candidates
        .iter()
        .find(|c| c.label == "/simplify")
        .expect("skill");
    assert!(simplify.detail.contains("after a refactor"));
    transport.shutdown().await;
}

#[tokio::test]
async fn the_subagent_catalog_separates_children_from_diagnostics() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;

    app.load_subagents(&mut transport);
    let listed = drive(&mut app, &mut transport, &mut incoming, |app| {
        !app.subagents.is_empty()
    })
    .await;
    assert!(listed, "expected the subagent catalog");

    let children = app.subagents.children();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].title(), "reviewer");
    assert!(children[0].can_prompt(app.subagents.parent_available));
    // An inactive one-shot child is not resident; that is not the same as finished.
    assert_eq!(children[1].status(), "not resident");
    assert!(!children[1].can_prompt(app.subagents.parent_available));

    let diagnostics = app.subagents.diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert!(diagnostics[0].is_transient());
    transport.shutdown().await;
}

#[tokio::test]
async fn the_sidebar_titles_come_from_the_title_projection() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| app.sessions.len() == 3).await;

    // A SessionSummary carries no title; the display title is the `title` projection.
    assert_eq!(app.sessions[0].title, "Wire the trajectory pane");
    assert!(app.sessions[0].running);
    // A blank session has not been used yet.
    assert_eq!(app.sessions[2].title, "New session");
    transport.shutdown().await;
}

#[tokio::test]
async fn the_preset_roster_lists_broken_presets_without_offering_them() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;

    app.load_presets(&mut transport);
    let listed = drive(&mut app, &mut transport, &mut incoming, |app| {
        !app.preset_roster.presets.is_empty()
    })
    .await;
    assert!(listed, "expected the preset roster");

    // Hiding a broken preset would turn "misconfigured" into "gone".
    assert_eq!(app.preset_roster.presets.len(), 3);
    assert_eq!(app.preset_roster.selectable().len(), 2);
    assert_eq!(
        app.preset_roster.presets[2].unusable_reason(),
        Some("missing tool: dsh-tool-bash")
    );
    assert_eq!(
        app.preset_roster.default_preset().map(|p| p.id.as_str()),
        Some("coding")
    );
    transport.shutdown().await;
}

#[tokio::test]
async fn a_custom_permission_value_is_shown_but_never_offered() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| app.permissions().is_some()).await;

    let permissions = app.permissions().expect("permission select");
    assert!(permissions.is_custom());
    assert_eq!(permissions.current_label(), "Custom");
    // `custom` is derived from knobs matching no preset; there is no table entry to
    // switch to, so it is not a choice.
    let switchable: Vec<_> = permissions
        .switchable()
        .iter()
        .map(|option| option.value.clone())
        .collect();
    assert_eq!(switchable, vec!["safe", "yolo"]);
    transport.shutdown().await;
}

#[tokio::test]
async fn workflow_runs_and_produced_files_fold_from_the_transcript() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    let ready = drive(&mut app, &mut transport, &mut incoming, |app| {
        !app.workflow_runs().is_empty() && !app.produced_files().is_empty()
    })
    .await;
    assert!(ready, "expected a workflow run and a produced file");

    let runs = app.workflow_runs();
    assert_eq!(runs[0].name, "review-changes");
    assert!(runs[0].is_running());
    assert_eq!(runs[0].members[0].status(), "completed");
    assert_eq!(runs[0].phases()[0].0.as_deref(), Some("Review"));

    // The no-op edit (old == new) changed nothing and must not be listed.
    assert_eq!(app.produced_files(), vec!["src/parse.rs"]);
    transport.shutdown().await;
}

#[tokio::test]
async fn the_directory_picker_navigates_by_host_paths() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;

    app.browse_directory(None, &mut transport);
    let listed = drive(&mut app, &mut transport, &mut incoming, |app| app.picker.is_some()).await;
    assert!(listed, "expected a directory listing");

    let picker = app.picker.as_ref().expect("picker");
    assert_eq!(picker.breadcrumb(), "~/projects");
    // Hidden rows are the client's choice and start hidden.
    assert_eq!(picker.rows().len(), 2);
    // The host cut the listing; the note must not imply hidden rows are the cause.
    assert!(picker.truncation_note().is_some());
    // The cursor opens on the synthetic "use this directory" row, whose target is the
    // directory being browsed rather than any entry.
    assert!(picker.on_use_row());
    assert_eq!(picker.chosen_path(), Some("/home/acp/projects"));
    assert_eq!(picker.parent_path(), Some("/home/acp"));

    // One step down reaches the first entry, and navigation uses the host's path verbatim.
    let picker = app.picker.as_mut().expect("picker");
    picker.select_next();
    assert_eq!(picker.selected_path(), Some("/home/acp/projects/dsh-tui"));
    assert_eq!(picker.chosen_path(), Some("/home/acp/projects/dsh-tui"));
    transport.shutdown().await;
}

#[tokio::test]
async fn attachments_are_checked_against_the_hosts_limits_before_sending() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| app.image_limits().is_some()).await;

    let png = |name: &str, bytes: usize| dsh_tui::attachment::Draft {
        name: name.into(),
        media_type: "image/png".into(),
        data: vec![0u8; bytes],
        dimensions: Some((800, 600)),
    };

    app.attach_image(png("one.png", 200_000));
    assert_eq!(app.drafts.len(), 1);
    assert!(app.attachment_error.is_none());

    // A third image exceeds the per-message count of 2.
    app.attach_image(png("two.png", 200_000));
    app.attach_image(png("three.png", 200_000));
    assert_eq!(app.drafts.len(), 2);
    assert!(app.attachment_error.as_deref().unwrap().contains("at most 2 images"));

    // A wrong media type names the accepted ones.
    app.clear_drafts();
    let gif = dsh_tui::attachment::Draft {
        media_type: "image/gif".into(),
        ..png("a.gif", 10)
    };
    app.attach_image(gif);
    let error = app.attachment_error.as_deref().unwrap();
    assert!(error.contains("not an accepted image type"));
    assert!(error.contains("image/png"));
    transport.shutdown().await;
}

#[tokio::test]
async fn a_plan_review_takes_over_the_composer_and_never_infers_approval() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn_with_env(&program, &args, &[("DSH_TUI_STUB_ASK", "plan")])
            .expect("stub should spawn");
    let mut app = App::new(Theme::default());

    let arrived = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.questions.is_some()
    })
    .await;
    assert!(arrived, "expected the question takeover");
    // It uses its own surface, not the generic approval modal.
    assert!(app.asks.is_empty());

    let pending = app.questions.as_ref().expect("pending");
    let question = pending.current().expect("question");
    // "Reject" is listed first; approval is named, so order must not decide the verdict.
    assert_eq!(question.options[0].label, "Reject");
    assert_eq!(question.plan_review_approve().as_deref(), Some("Approve"));
    assert!(question.approves("Approve"));
    assert!(!question.approves("Reject"));

    // Nothing is submittable until an option is chosen.
    assert!(!pending.is_complete());
    app.next_option();
    app.choose_option();
    assert!(app.questions.as_ref().unwrap().is_complete());

    app.submit_questions(&mut transport);
    let settled = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.log.iter().any(|line| line.contains("resolved via answer"))
    })
    .await;
    assert!(settled, "the stub should confirm the answer: {:?}", app.log);
    assert!(app.questions.is_none());
    transport.shutdown().await;
}

#[tokio::test]
async fn a_question_request_can_be_delegated_instead_of_answered() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn_with_env(&program, &args, &[("DSH_TUI_STUB_ASK", "plan")])
            .expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| app.questions.is_some()).await;

    app.delegate_questions(&mut transport);
    let delegated = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.log.iter().any(|line| line.contains("resolved via next"))
    })
    .await;
    // Delegating hands the request to the host's answerer rather than inventing a reply.
    assert!(delegated, "expected delegation: {:?}", app.log);
    assert!(app.questions.is_none());
    transport.shutdown().await;
}

#[tokio::test]
async fn scrolling_to_the_top_backfills_older_history() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    let opened = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.ledger.has_more() && app.ledger.cursor().is_some()
    })
    .await;
    assert!(opened, "expected the opening frame's cut and hasMore");

    let before = app.ledger.span().expect("a span");
    // Reaching the top asks for the page immediately older than what is held.
    app.measure(20);
    app.scroll_conversation(dsh_tui::app::ScrollDelta::Top, &mut transport);

    let backfilled = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.ledger.span().map(|(start, _)| start) != Some(before.0)
    })
    .await;
    assert!(backfilled, "expected older records to be prepended");

    let after = app.ledger.span().expect("a span");
    assert!(after.0 < before.0, "the ledger should now start earlier");
    // The newest end is untouched: a backwards page only extends the front.
    assert_eq!(after.1, before.1);
    // No gap or overlap was reported.
    assert!(!app.log.iter().any(|line| line.contains("rejected")), "{:?}", app.log);
    transport.shutdown().await;
}

#[tokio::test]
async fn a_page_must_quote_the_follow_frames_cut() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| app.ledger.cursor().is_some()).await;

    // The cut comes from the opening frame; the stub rejects anything else, which is what
    // keeps page boundaries from sliding as live events append.
    assert_eq!(app.ledger.cursor(), Some(100));

    let id = transport
        .call(
            "session",
            "page",
            serde_json::json!({ "address": { "sessionId": "s-1" }, "throughSeq": 999 }),
        )
        .expect("send");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut code = None;
    while let Ok(Some(message)) = tokio::time::timeout_at(deadline, incoming.recv()).await {
        if let dsh_tui::transport::Incoming::Msg(message) = message {
            if let dsh_tui_proto::ServerMsg::Err { id: got, code: c, .. } = *message {
                if got == id {
                    code = Some(c);
                    break;
                }
            }
        }
    }
    assert_eq!(code.as_deref(), Some("bad-request"));
    transport.shutdown().await;
}

#[tokio::test]
async fn a_run_code_program_shows_its_nested_dispatches() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    let ready = drive(&mut app, &mut transport, &mut incoming, |app| {
        !dsh_tui::calltree::sub_calls(&app.ledger).is_empty()
    })
    .await;
    assert!(ready, "expected the nested dispatches");

    let tree = dsh_tui::calltree::build(&app.ledger);
    let rows = dsh_tui::calltree::rows(&tree);

    // The run_code root, its bash sub-call, and the read_file nested under that.
    let program_row = rows
        .iter()
        .position(|row| row.label.starts_with("run_code"))
        .expect("the run_code row");
    assert_eq!(rows[program_row].depth, 0);
    assert_eq!(rows[program_row + 1].label, "bash  ls -la");
    assert_eq!(rows[program_row + 1].depth, 1);
    // parentCallId named the bash sub-call, so this nests one level deeper.
    assert_eq!(rows[program_row + 2].label, "read_file  src/lex.rs");
    assert_eq!(rows[program_row + 2].depth, 2);
    // It never settled, so it is running rather than lost.
    assert_eq!(rows[program_row + 2].status, "running");

    let subs = dsh_tui::calltree::sub_calls(&app.ledger);
    assert_eq!(subs[0].millis, Some(100));
    transport.shutdown().await;
}

#[tokio::test]
async fn the_measured_line_list_matches_what_is_rendered() {
    // The viewport's offsets and the search's match indices address the measured list, so
    // a drift between it and the drawn lines would scroll and highlight the wrong rows.
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| {
        !dsh_tui::calltree::sub_calls(&app.ledger).is_empty()
    })
    .await;

    // A pane tall enough to hold the whole transcript.
    app.measure(200);
    let measured = app.rendered.len();

    let mut terminal = Terminal::new(TestBackend::new(120, 200)).expect("backend");
    terminal
        .draw(|frame| dsh_tui::ui::render(frame, &app))
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    // Count drawn conversation lines: rows inside the centre pane that carry text.
    let drawn = (1..buffer.area.height - 1)
        .filter(|y| {
            let row: String = (31..115)
                .map(|x| buffer[(x, *y)].symbol().to_string())
                .collect();
            !row.trim().is_empty()
        })
        .count();

    // Blank separator lines are not drawn, so the measured list is the larger of the two;
    // what matters is that measuring never reports fewer lines than are drawn.
    assert!(
        measured >= drawn,
        "measured {measured} lines but drew {drawn}"
    );
    assert!(measured > 0 && drawn > 0);
    transport.shutdown().await;
}

#[tokio::test]
async fn a_prompt_carries_a_client_minted_request_id() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| !app.sessions.is_empty()).await;

    for ch in "hello".chars() {
        app.composer.insert(ch);
    }
    app.submit_prompt(&mut transport);
    // The composer clears on submit rather than waiting for the receipt.
    assert!(app.composer.is_empty());

    // The stub rejects a prompt missing `requestId` or `content`, so no failure logged
    // means the real request shape was sent.
    let settled = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.log.iter().any(|line| line.contains("failed"))
    })
    .await;
    assert!(!settled, "prompt was rejected: {:?}", app.log);
    transport.shutdown().await;
}

#[tokio::test]
async fn creating_a_session_selects_it_once_the_list_refreshes() {
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| app.sessions.len() == 3).await;

    // A session is rooted in a chosen workspace, so one has to be chosen first.
    app.workspace = Some("/home/acp/projects/dsh-tui".to_string());
    app.new_session(&mut transport);
    let created = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.sessions.len() == 4
    })
    .await;
    assert!(created, "expected the new session in the list");
    // The cursor follows the new session rather than staying where it was.
    assert_eq!(app.active_session_id(), Some("s-4"));
    transport.shutdown().await;
}

#[tokio::test]
async fn every_exchange_is_logged_from_both_ends_under_one_correlation_key() {
    // The point of the log is answering "what happened to this request". That only works
    // if a request, the bridge's handling of it, and the reply all carry the same key.
    use dsh_tui::logging::{Origin, Source};

    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());

    let ok = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.log.iter().any(|record| record.event == "call.ok")
    })
    .await;
    assert!(ok, "expected a call to complete: {:?}", app.log);

    let request = app
        .log
        .iter()
        .find(|record| record.event == "call.out")
        .expect("the outgoing call should be recorded");
    let id = request.id.expect("a call is an exchange and carries its id");
    assert_eq!(request.origin, Some(Origin::Client));
    assert_eq!(request.src, Source::Tui);
    assert_eq!(request.fields.get("ns").and_then(|v| v.as_str()), Some("session"));

    let reply = app
        .log
        .iter()
        .find(|record| record.event == "call.ok" && record.id == Some(id))
        .expect("the reply should carry the request's id");
    assert_eq!(reply.origin, Some(Origin::Client));
    // The duration is the whole reason the transport times exchanges rather than either
    // peer: this is the only place that sees both halves.
    assert!(reply.fields.contains_key("ms"), "a settled call should be timed");
    // The originating method travels with the reply, which the wire frame does not carry.
    assert!(reply.message.contains("session.list"), "got {:?}", reply.message);

    // Filtering by the exchange finds both halves and nothing unrelated.
    let both: Vec<_> = app
        .log
        .iter()
        .filter(|record| record.contains(&format!("c{id}")))
        .collect();
    assert!(both.len() >= 2, "expected the exchange's records: {both:?}");
    assert!(both.iter().all(|record| record.id == Some(id)));

    transport.shutdown().await;
}

#[tokio::test]
async fn the_log_pane_narrows_without_showing_records_the_file_never_took() {
    use dsh_tui::app::SearchEdit;
    use dsh_tui::logging::Level;

    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| {
        app.log.iter().any(|record| record.event == "call.ok")
    })
    .await;

    app.open_logs();
    let all = app.visible_logs().len();
    assert!(all > 0, "the pane should show the run's records");

    // A filter narrows; clearing it restores.
    app.edit_log_filter(SearchEdit::Push('z'));
    app.edit_log_filter(SearchEdit::Push('z'));
    assert_eq!(app.visible_logs().len(), 0);
    app.edit_log_filter(SearchEdit::Clear);
    assert_eq!(app.visible_logs().len(), all);

    // Narrowing the level can only ever hide records, never reveal one, and errors
    // survive every threshold.
    app.log_min = Level::Error;
    let errors = app.visible_logs();
    assert!(errors.len() <= all);
    assert!(errors.iter().all(|record| record.level == Level::Error));

    transport.shutdown().await;
}

#[tokio::test]
async fn a_session_cannot_be_created_before_a_workspace_is_chosen() {
    // The agent's working directory decides where its edits land. Guessing it from the
    // process cwd puts another project's files wherever the binary was launched from.
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| app.sessions.len() == 3).await;

    assert!(app.workspace.is_none(), "a run starts with no workspace");
    app.new_session(&mut transport);

    // The picker opens instead of a session being created against a guessed directory.
    let asked = drive(&mut app, &mut transport, &mut incoming, |app| app.picker.is_some()).await;
    assert!(asked, "expected the directory picker: {:?}", app.log);
    assert_eq!(app.sessions.len(), 3, "no session may exist yet");

    // Choosing registers the directory with the host, then resumes the creation.
    app.choose_directory(&mut transport);
    assert!(app.picker.is_none(), "choosing closes the picker");
    let registered = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspace.is_some()
    })
    .await;
    assert!(registered, "the host should register the workspace: {:?}", app.log);
    let chosen = app.workspace.clone().expect("choosing sets the workspace");
    let created = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.sessions.len() == 4
    })
    .await;
    assert!(created, "the deferred creation should resume: {:?}", app.log);

    // The log names the directory, so a session's root is answerable after the fact.
    let record = app
        .log
        .iter()
        .find(|record| record.event == "session.create")
        .expect("the creation should be recorded");
    assert_eq!(
        record.fields.get("cwd").and_then(|v| v.as_str()),
        Some(chosen.as_str())
    );
    transport.shutdown().await;
}

#[tokio::test]
async fn dismissing_the_picker_abandons_the_creation_it_opened_for() {
    // Otherwise the next directory browsed for any other reason would silently create a
    // session the human never asked for.
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| app.sessions.len() == 3).await;

    app.new_session(&mut transport);
    drive(&mut app, &mut transport, &mut incoming, |app| app.picker.is_some()).await;
    app.cancel_picker();
    assert!(app.workspace.is_none());

    // Browsing again, and choosing, must not resurrect the abandoned creation.
    app.browse_directory(None, &mut transport);
    drive(&mut app, &mut transport, &mut incoming, |app| app.picker.is_some()).await;
    app.choose_directory(&mut transport);
    let set = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspace.is_some()
    })
    .await;
    assert!(set, "choosing still sets the workspace");
    assert_eq!(app.sessions.len(), 3, "no session should have been created");
    transport.shutdown().await;
}

#[tokio::test]
async fn an_existing_workspace_can_be_selected_from_the_list() {
    // `^w` used to be a display with no keys: it could show a workspace but never use
    // one, so the only way to root a session was to re-browse to a path by hand.
    use dsh_tui::app::View;

    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| app.sessions.len() == 3).await;

    app.open_workspace_list(&mut transport);
    let listed = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspaces.rows().len() == 2
    })
    .await;
    assert!(listed, "the workspace stream should fill the list");

    // The cursor moves and stops at the ends rather than wrapping past them.
    assert_eq!(app.workspace_row, 0);
    app.select_next_workspace();
    assert_eq!(app.workspace_row, 1);
    app.select_next_workspace();
    assert_eq!(app.workspace_row, 1, "the cursor must not run past the last row");
    app.select_prev_workspace();
    assert_eq!(app.workspace_row, 0);

    let expected = app.workspaces.rows()[0].path.clone();
    app.use_selected_workspace(&mut transport);

    // A workspace the host already knows needs no registration round trip.
    assert_eq!(app.workspace.as_deref(), Some(expected.as_str()));
    assert_eq!(app.view, View::Conversation, "using one returns to the conversation");

    // And it is now usable as a session root without the picker opening.
    app.new_session(&mut transport);
    assert!(app.picker.is_none(), "a chosen workspace must not re-prompt");
    let created = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.sessions.len() == 4
    })
    .await;
    assert!(created, "expected the session: {:?}", app.log);
    transport.shutdown().await;
}

#[tokio::test]
async fn choosing_a_directory_registers_it_as_a_workspace() {
    // A path held only in memory is forgotten at exit and invisible to `^w`. Going
    // through `workspace/create` is what makes the choice outlive the run.
    let (program, args) = stub();
    let (mut transport, mut incoming) =
        Transport::spawn(&program, &args).expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| app.sessions.len() == 3).await;

    app.browse_directory(None, &mut transport);
    drive(&mut app, &mut transport, &mut incoming, |app| app.picker.is_some()).await;
    app.choose_directory(&mut transport);
    let done = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspace.is_some()
    })
    .await;
    assert!(done, "expected the registration to settle: {:?}", app.log);

    let registered = app
        .log
        .iter()
        .find(|record| record.event == "workspace.registered")
        .expect("registration should be recorded");
    // `created` distinguishes a new workspace from re-picking a known one; both are fine.
    assert!(registered.fields.contains_key("created"));
    // The host's canonical path wins over the string the client sent.
    assert_eq!(
        registered.fields.get("path").and_then(|v| v.as_str()),
        app.workspace.as_deref()
    );
    transport.shutdown().await;
}

#[tokio::test]
async fn a_harness_without_workspace_create_still_lets_you_work() {
    // Registration is best effort. A harness that does not expose the method must not
    // leave the human unable to start a session at all — but it must say so, because the
    // choice will not survive the run.
    let (program, args) = stub();
    let (mut transport, mut incoming) = Transport::spawn_with_env(
        &program,
        &args,
        &[("DSH_TUI_STUB_NO_WORKSPACE_CREATE", "1")],
    )
    .expect("stub should spawn");
    let mut app = App::new(Theme::default());
    drive(&mut app, &mut transport, &mut incoming, |app| app.sessions.len() == 3).await;

    app.new_session(&mut transport);
    drive(&mut app, &mut transport, &mut incoming, |app| app.picker.is_some()).await;
    app.choose_directory(&mut transport);

    let recovered = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.workspace.is_some()
    })
    .await;
    assert!(recovered, "the rejection must not strand the human: {:?}", app.log);
    assert!(
        app.log.iter().any(|r| r.event == "workspace.register.failed"),
        "the lost persistence should be reported: {:?}", app.log
    );
    // The deferred session creation still resumes.
    let created = drive(&mut app, &mut transport, &mut incoming, |app| {
        app.sessions.len() == 4
    })
    .await;
    assert!(created, "expected the session anyway: {:?}", app.log);
    transport.shutdown().await;
}
