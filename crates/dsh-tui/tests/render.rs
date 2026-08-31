//! Deterministic render tests over a fixed-size backend.

use dsh_tui::app::{App, Connection, PendingAsk, SessionRow};
use dsh_tui::theme::Theme;
use dsh_tui::ui;
use dsh_tui_proto::Ready;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn ready_app() -> App {
    let mut app = App::new(Theme::default());
    app.connection = Connection::Ready(Box::new(Ready {
        protocol: dsh_tui_proto::PROTOCOL_VERSION,
        client_id: Some("test".into()),
        host: None,
        namespaces: vec!["session".into(), "workspace".into(), "settings".into()],
        events: vec!["approval/request".into(), "user-questions/request".into()],
    }));
    app.sessions = vec![
        SessionRow { id: "s-1".into(), title: "Wire the trajectory pane".into(),
                     running: true, cwd: None, is_subagent: false },
        SessionRow { id: "s-2".into(), title: "Port the approval modal".into(),
                     running: false, cwd: None, is_subagent: false },
    ];
    app
}

fn render_to_text(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("test backend");
    terminal.draw(|frame| ui::render(frame, app)).expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            let mut row = String::new();
            let mut skip = 0u16;
            for x in 0..buffer.area.width {
                // A double-width glyph occupies two cells; the second holds filler that
                // would otherwise be read back as a space inside the word.
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let symbol = buffer[(x, y)].symbol();
                skip = symbol.chars().map(display_width).max().unwrap_or(1) - 1;
                row.push_str(symbol);
            }
            row.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Terminal columns one character occupies.
///
/// Only the ranges this UI actually renders need covering: CJK and full-width forms are
/// two columns wide, everything else here is one.
fn display_width(ch: char) -> u16 {
    let code = ch as u32;
    let wide = (0x1100..=0x115F).contains(&code)
        || (0x2E80..=0xA4CF).contains(&code)
        || (0xAC00..=0xD7A3).contains(&code)
        || (0xF900..=0xFAFF).contains(&code)
        || (0xFE30..=0xFE6F).contains(&code)
        || (0xFF00..=0xFF60).contains(&code)
        || (0xFFE0..=0xFFE6).contains(&code);
    if wide {
        2
    } else {
        1
    }
}

#[test]
fn the_frame_shows_both_columns_and_the_status_bar() {
    let text = render_to_text(&ready_app(), 100, 20);
    println!("{text}");
    assert!(text.contains("Sessions"));
    assert!(text.contains("Conversation"));
    assert!(text.contains("Wire the trajectory pane"));
    assert!(text.contains("connected"));
    assert!(text.contains("Ask anything"));
    // Details is closed by default and must not take a seat.
    assert!(!text.contains("Details"));
}

#[test]
fn collapsing_the_sidebar_gives_its_width_to_the_conversation() {
    let mut app = ready_app();
    app.toggle_sidebar();
    let text = render_to_text(&app, 100, 20);
    assert!(!text.contains("Sessions"));
    assert!(text.contains("Conversation"));
}

#[test]
fn a_blocked_waterfall_takes_the_screen() {
    let mut app = ready_app();
    app.asks.push_back(PendingAsk {
        id: 1,
        event: "approval/request".into(),
        args: vec![serde_json::json!({ "title": "Run `rm -rf build/`" })],
    });
    let text = render_to_text(&app, 100, 20);
    println!("{text}");
    assert!(text.contains("Permission required"));
    assert!(text.contains("Run `rm -rf build/`"));
    assert!(text.contains("allow"));
    assert!(text.contains("delegate to host"));
}

#[test]
fn an_opaque_waterfall_offers_only_delegation() {
    let mut app = ready_app();
    app.asks.push_back(PendingAsk {
        id: 2,
        event: "approval/request".into(),
        args: vec![serde_json::json!({ "somethingNew": true })],
    });
    let text = render_to_text(&app, 100, 20);
    // Offering allow/deny for a request the build cannot render would put the user's name
    // on a decision they could not actually read.
    assert!(text.contains("cannot render this request"));
    assert!(text.contains("only safe reply"));
    assert!(!text.contains(" allow "));
}

#[test]
fn the_modal_stays_inside_a_tiny_terminal() {
    // A floor above the available size would push the popup off-screen; on a cramped
    // terminal the modal must still fit rather than draw outside the frame.
    let mut app = ready_app();
    app.asks.push_back(PendingAsk {
        id: 3,
        event: "approval/request".into(),
        args: vec![serde_json::json!({ "title": "tiny" })],
    });
    let text = render_to_text(&app, 12, 4);
    assert!(!text.is_empty());
}


#[test]
fn the_candidate_menu_floats_above_the_composer() {
    let mut app = ready_app();
    app.focus = dsh_tui::app::Pane::Conversation;
    for ch in "/mo".chars() {
        app.composer.insert(ch);
    }
    app.candidates = vec![
        dsh_tui::app::Candidate {
            label: "/model".into(),
            detail: "Choose the conversation model".into(),
            insert: "/model".into(),
        },
        dsh_tui::app::Candidate {
            label: "/compact".into(),
            detail: "Compact the transcript".into(),
            insert: "/compact".into(),
        },
    ];
    let text = render_to_text(&app, 100, 20);
    println!("{text}");
    // The query narrows the menu to the matching row.
    assert!(text.contains("/model"));
    assert!(!text.contains("/compact"));
    assert!(text.contains("Choose the conversation model"));
}

#[test]
fn the_settings_form_renders_namespaces_fields_and_overrides() {
    let mut app = ready_app();
    app.view = dsh_tui::app::View::Settings;
    app.settings = Some(
        serde_json::from_value(serde_json::json!({
            "writable": true,
            "hasDocument": true,
            "namespaces": [{
                "ns": "llm-deepseek",
                "schema": { "type": "object", "dict": {
                    "baseUrl": { "type": "string", "meta": { "description": "API base URL" } },
                    "apiKey": { "type": "string" }
                } },
                "value": { "baseUrl": "https://api.deepseek.com" },
                "user": { "baseUrl": "https://api.deepseek.com" },
                "applies": "restart",
                "secrets": [{ "path": ["apiKey"], "set": true }],
                "revision": 3
            }]
        }))
        .expect("describe"),
    );
    let text = render_to_text(&app, 100, 20);
    println!("{text}");
    assert!(text.contains("llm-deepseek"));
    assert!(text.contains("baseUrl"));
    assert!(text.contains("·overridden"));
    assert!(text.contains("Changes apply after a restart"));
    // The secret's value must never be rendered.
    assert!(text.contains("••••••••"));
    assert!(!text.contains("api.deepseek.com/secret"));
}

#[test]
fn a_read_only_provider_says_so_before_an_edit_fails() {
    let mut app = ready_app();
    app.view = dsh_tui::app::View::Settings;
    app.settings = Some(
        serde_json::from_value(serde_json::json!({
            "writable": false, "hasDocument": false, "namespaces": []
        }))
        .expect("describe"),
    );
    let text = render_to_text(&app, 100, 12);
    assert!(text.contains("read-only"));
}

#[test]
fn the_workspace_browser_groups_sessions_under_their_workspace() {
    let mut app = ready_app();
    app.view = dsh_tui::app::View::Workspace;
    app.workspaces.apply(
        1,
        serde_json::from_value(serde_json::json!({
            "type": "baseline",
            "value": {
                "items": [{ "workspaceId": "w-1", "path": "/home/acp/dsh-tui",
                            "title": "dsh-tui", "sessionIds": ["s-1", "s-2"],
                            "createdAt": "", "updatedAt": "" }],
                "archivedSessionIds": ["s-2"]
            }
        }))
        .expect("baseline"),
    );
    let text = render_to_text(&app, 100, 12);
    println!("{text}");
    assert!(text.contains("dsh-tui"));
    assert!(text.contains("s-1"));
    // An archived session must not show under its workspace.
    assert!(!text.contains("s-2"));
}

#[test]
fn settings_never_uses_replace() {
    // The settings view is redacted: every `role('secret')` field is stripped before it
    // reaches the wire. A section rebuilt from that view and sent through `replace` would
    // silently delete every secret the wire never returned, so the form must only ever
    // emit path-addressed `mutate` ops.
    for file in ["../src/app.rs", "../src/settings.rs"] {
        let source = std::fs::read_to_string(format!("{}/tests/{file}", env!("CARGO_MANIFEST_DIR")))
            .expect("source file");
        assert!(
            !source.contains("\"replace\""),
            "{file} names the settings `replace` endpoint"
        );
    }
}

#[test]
fn the_models_page_shows_readiness_per_route() {
    let mut app = ready_app();
    app.view = dsh_tui::app::View::Settings;
    app.settings_section = dsh_tui::app::SettingsSection::Models;
    app.settings = Some(
        serde_json::from_value(serde_json::json!({
            "writable": true, "hasDocument": true,
            "namespaces": [{ "ns": "llm-deepseek", "schema": {}, "value": {},
                             "applies": "live", "secrets": [], "revision": 1 }]
        }))
        .expect("describe"),
    );
    app.providers = serde_json::from_value(serde_json::json!([
        { "id": "deepseek-official", "name": "DeepSeek" }
    ]))
    .expect("providers");
    app.configurable = serde_json::from_value(serde_json::json!([
        { "provider": "deepseek-official", "displayName": "DeepSeek",
          "settingsNs": "llm-deepseek", "settingsPath": [] },
        { "provider": "anthropic", "displayName": "Anthropic",
          "settingsNs": "llm-deepseek", "settingsPath": [] }
    ]))
    .expect("configurable");
    app.credentials.insert(
        "DEEPSEEK_OFFICIAL_API_KEY".into(),
        serde_json::from_value(serde_json::json!({
            "configured": true, "source": "env", "writable": true
        }))
        .expect("credential"),
    );

    let text = render_to_text(&app, 100, 16);
    println!("{text}");
    assert!(text.contains("Settings · Models"));
    assert!(text.contains("DeepSeek"));
    assert!(text.contains("ready"));
    // Registered but uncredentialed reads differently from unregistered.
    assert!(text.contains("Anthropic"));
    assert!(text.contains("adapter not registered") || text.contains("needs an API key"));
    // The selected row discloses which reference it resolves through.
    assert!(text.contains("DEEPSEEK_OFFICIAL_API_KEY"));
}

#[test]
fn the_inventory_page_shows_search_summary_and_raw_unknown_phases() {
    let mut app = ready_app();
    app.view = dsh_tui::app::View::Settings;
    app.settings_section = dsh_tui::app::SettingsSection::Plugins;
    app.inventory.replace(
        serde_json::from_value(serde_json::json!({
            "entries": [
                { "entryId": "e1", "moduleName": "@deepseek-ai/dsh-tool-bash",
                  "enabled": true, "fiberPhase": "active" },
                { "entryId": "e2", "moduleName": "@deepseek-ai/dsh-goal",
                  "enabled": true, "fiberPhase": "failed" },
                { "entryId": "e3", "moduleName": "@deepseek-ai/dsh-schedule",
                  "enabled": true, "fiberPhase": "quiescing" }
            ]
        }))
        .expect("snapshot"),
    );
    let text = render_to_text(&app, 100, 14);
    println!("{text}");
    assert!(text.contains("Settings · Plugins"));
    assert!(text.contains("3 entries · 1 active"));
    assert!(text.contains("1 need attention"));
    assert!(text.contains("dsh-tool-bash"));
    assert!(text.contains("failed"));
    // An upstream phase this build does not know is shown verbatim, not swallowed.
    assert!(text.contains("quiescing"));
}

#[test]
fn the_general_page_explains_how_the_theme_resolved() {
    let mut app = ready_app();
    app.view = dsh_tui::app::View::Settings;
    app.settings_section = dsh_tui::app::SettingsSection::General;
    app.theme_preference = dsh_tui::theme::Preference::System;
    app.theme_reason = "terminal does not report a theme; using dark";

    let text = render_to_text(&app, 100, 12);
    println!("{text}");
    assert!(text.contains("Settings · General"));
    assert!(text.contains("Appearance"));
    assert!(text.contains("system"));
    // The row must not imply the terminal was consulted and answered.
    assert!(text.contains("does not report a theme"));
    // Font size is the terminal's, and the page says so rather than offering a dead control.
    assert!(text.contains("owned by your terminal"));
}

#[test]
fn the_trajectory_pane_shows_turns_tools_and_bars() {
    use dsh_tui_proto::{HistoryRecord, JournalChange, JournalItem, SessionEvent};

    fn event(kind: &str, seq: u64, time: i64, data: serde_json::Value) -> HistoryRecord {
        HistoryRecord::Event {
            event: SessionEvent { kind: kind.into(), seq, time: Some(time), data },
        }
    }

    let mut app = ready_app();
    app.details_open = true;
    app.ledger.apply(
        1,
        &JournalItem::delta(
            JournalChange::Replace,
            vec![
                event("turn/start", 1, 0, serde_json::json!({})),
                event("tool/call", 2, 100, serde_json::json!({
                    "turn": 1, "step": 0, "callId": "c1", "name": "read_file",
                    "arguments": r#"{"path":"src/lex.rs"}"#
                })),
                event("tool/result", 3, 1_350, serde_json::json!({
                    "turn": 1, "step": 0,
                    "message": { "content": [{ "toolCallId": "c1", "content": [], "isError": true }] }
                })),
                event("turn/end", 4, 2_000, serde_json::json!({})),
                event("turn/start", 5, 2_100, serde_json::json!({})),
            ],
        ),
    );

    let text = render_to_text(&app, 120, 16);
    println!("{text}");
    assert!(text.contains("Trajectory"));
    assert!(text.contains("turn 1"));
    assert!(text.contains("2.0s"));
    assert!(text.contains("read_file"));
    assert!(text.contains("1.2s") || text.contains("1.3s"));
    assert!(text.contains("failed"));
    // The open second turn reads as running, not as zero.
    assert!(text.contains("running"));
    assert!(text.contains('█'));
}

#[test]
fn the_chip_strip_shows_goal_plan_and_jobs() {
    let mut app = ready_app();
    app.focus = dsh_tui::app::Pane::Conversation;
    app.control.apply(
        1,
        serde_json::from_value(serde_json::json!({
            "type": "baseline",
            "value": {
                "queues": {},
                "jobs": { "s-1": [{ "id": "j1", "status": "running", "startedAt": 1 }] },
                "projections": { "s-1": {
                    "plan": { "active": true, "pending": false },
                    "goal": { "objective": "Ship the TUI", "phase": "active",
                              "activation": "disarmed", "maxGoalRounds": 20,
                              "roundsStarted": 3 }
                } }
            }
        }))
        .expect("baseline"),
    );
    // ready_app's first session is s-1.
    app.sessions[0].id = "s-1".into();

    let text = render_to_text(&app, 110, 18);
    println!("{text}");
    assert!(text.contains("Ship the TUI · active · 3/20"));
    // A disarmed process will not continue the goal; the chip must say so.
    assert!(text.contains("(disarmed)"));
    assert!(text.contains("plan on"));
    assert!(text.contains("1 job"));
}

#[test]
fn the_model_picker_names_providers_that_are_missing_from_the_list() {
    let mut app = ready_app();
    app.focus = dsh_tui::app::Pane::Conversation;
    app.catalog = dsh_tui::chips::Catalog::read(&serde_json::json!({
        "default": { "provider": "deepseek-official", "model": "deepseek-v4-pro" },
        "routableProviders": ["deepseek-official", "anthropic", "empty-gw"],
        "groups": [
            { "id": "deepseek-official", "name": "DeepSeek",
              "models": [{ "id": "deepseek-v4-pro", "name": "V4 Pro" }] },
            { "id": "empty-gw", "name": "Empty Gateway", "models": [] }
        ],
        "failures": [{ "id": "anthropic", "name": "Anthropic", "message": "401 unauthorized" }]
    }));
    app.model_picker = Some(String::new());

    let text = render_to_text(&app, 110, 18);
    println!("{text}");
    assert!(text.contains("DeepSeek · V4 Pro"));
    // A failed provider and an empty one are explained rather than silently absent.
    assert!(text.contains("Anthropic unavailable: 401 unauthorized"));
    assert!(text.contains("empty-gw listed no models"));
}

#[test]
fn the_trajectory_pane_shows_workflows_and_produced_files() {
    use dsh_tui_proto::{HistoryRecord, JournalChange, JournalItem, SessionEvent};

    fn event(kind: &str, seq: u64, data: serde_json::Value) -> HistoryRecord {
        HistoryRecord::Event {
            event: SessionEvent { kind: kind.into(), seq, time: Some(0), data },
        }
    }

    let mut app = ready_app();
    app.details_open = true;
    app.ledger.apply(
        1,
        &JournalItem::delta(
            JournalChange::Replace,
            vec![
                event("tool-workflow/run-start", 1,
                      serde_json::json!({ "runId": "wf1", "name": "review-changes" })),
                event("tool-workflow/agent-start", 2, serde_json::json!({
                    "runId": "wf1", "seq": 1, "label": "review:bugs",
                    "phase": "Review", "childId": "k1" })),
                event("tool-workflow/agent-end", 3,
                      serde_json::json!({ "runId": "wf1", "seq": 1, "outcome": "failed" })),
                event("tool/call", 4, serde_json::json!({
                    "turn": 1, "step": 0, "callId": "c1", "name": "write",
                    "arguments": r#"{"file_path":"src/parse.rs","content":"fn parse(){}"}"# })),
                event("tool/result", 5, serde_json::json!({
                    "turn": 1, "step": 0,
                    "message": { "content": [{ "toolCallId": "c1", "content": [] }] } })),
            ],
        ),
    );

    let text = render_to_text(&app, 130, 18);
    println!("{text}");
    assert!(text.contains("workflow review-changes"));
    assert!(text.contains("Review"));
    assert!(text.contains("review:bugs"));
    assert!(text.contains("failed"));
    // The run itself has no run-end yet.
    assert!(text.contains("running"));
    assert!(text.contains("Produced"));
    assert!(text.contains("src/parse.rs"));
}

#[test]
fn the_sidebar_carries_a_brand_row() {
    let text = render_to_text(&ready_app(), 100, 16);
    assert!(text.contains("dsh-tui"));
    // Names the harness without adopting its trademark as this project's name.
    assert!(text.contains("built on DSH"));
}

#[test]
fn the_directory_picker_shows_its_breadcrumb_and_truncation_note() {
    let mut app = ready_app();
    let mut browser = dsh_tui::directory::Browser::new();
    browser.replace(
        serde_json::from_value(serde_json::json!({
            "path": "/home/acp/projects", "home": "/home/acp",
            "crumbs": [
                { "name": "/", "path": "/", "hidden": false },
                { "name": "acp", "path": "/home/acp", "hidden": false },
                { "name": "projects", "path": "/home/acp/projects", "hidden": false }
            ],
            "entries": [
                { "name": ".cache", "path": "/home/acp/projects/.cache", "hidden": true },
                { "name": "dsh-tui", "path": "/home/acp/projects/dsh-tui", "hidden": false }
            ],
            "truncated": true
        }))
        .expect("listing"),
    );
    app.picker = Some(browser);

    let text = render_to_text(&app, 100, 18);
    println!("{text}");
    assert!(text.contains("~/projects"));
    assert!(text.contains("dsh-tui"));
    // Hidden by default, and the truncation note does not blame hidden rows.
    assert!(!text.contains(".cache"));
    assert!(text.contains("more directories exist"));
}

#[test]
fn staged_images_say_when_they_cannot_be_drawn() {
    let mut app = ready_app();
    app.focus = dsh_tui::app::Pane::Conversation;
    app.graphics = dsh_tui::attachment::Graphics::None;
    app.drafts.push(dsh_tui::attachment::Draft {
        name: "shot.png".into(),
        media_type: "image/png".into(),
        data: vec![0u8; 1024],
        dimensions: Some((100, 100)),
    });

    let text = render_to_text(&app, 110, 18);
    println!("{text}");
    // A terminal that cannot draw names the file and the reason rather than blanking.
    assert!(text.contains("shot.png"));
    assert!(text.contains("cannot show images inline"));
}

#[test]
fn the_chrome_translates_into_chinese() {
    let mut app = ready_app();
    app.locale = dsh_tui::locale::Locale::Zh;
    let text = render_to_text(&app, 100, 16);
    println!("{text}");
    assert!(text.contains("会话"));
    assert!(text.contains("对话"));
    assert!(text.contains("暂无消息"));
    // The English chrome is gone, not merely supplemented.
    assert!(!text.contains("Conversation"));
}

#[test]
fn a_plan_review_marks_the_named_approving_option() {
    let mut app = ready_app();
    app.questions = Some(dsh_tui::app::PendingQuestions {
        id: 1,
        request: serde_json::from_value(serde_json::json!({ "questions": [{
            "id": "q1",
            "question": "Approve this plan?",
            "detail": "1. Wire the ledger",
            "options": [
                { "label": "Reject", "description": "Send it back" },
                { "label": "Approve", "description": "Proceed as written" }
            ],
            "intent": { "kind": "plan-review", "approve": "Approve" }
        }] }))
        .expect("request"),
        draft: dsh_tui::questions::Draft::new(),
        question_index: 0,
        option_index: 0,
    });

    let text = render_to_text(&app, 100, 18);
    println!("{text}");
    assert!(text.contains("Plan review"));
    assert!(text.contains("Approve this plan?"));
    assert!(text.contains("1. Wire the ledger"));
    assert!(text.contains("Reject"));
    assert!(text.contains("Approve"));
    // Nothing chosen yet, so submitting is not offered.
    assert!(!text.contains("enter submit"));
    assert!(text.contains("delegate"));
}

#[test]
fn scrolling_up_holds_position_while_new_lines_arrive() {
    use dsh_tui_proto::{HistoryRecord, JournalChange, JournalItem, SessionEvent};

    fn message(seq: u64, text: &str) -> HistoryRecord {
        HistoryRecord::Event {
            event: SessionEvent {
                kind: "assistant/message".into(),
                seq,
                time: Some(0),
                data: serde_json::json!({ "content": [{ "type": "text", "text": text }] }),
            },
        }
    }

    let mut app = ready_app();
    let records: Vec<_> = (1..=40).map(|n| message(n, &format!("line {n}"))).collect();
    app.ledger
        .apply(1, &JournalItem::delta(JournalChange::Replace, records));

    app.measure(20);
    let tail = render_to_text(&app, 100, 20);
    // A fresh view follows the newest line.
    assert!(tail.contains("line 40"));
    assert!(app.scroll.is_following());

    app.scroll.scroll_up(20);
    app.measure(20);
    let scrolled = render_to_text(&app, 100, 20);
    assert!(!scrolled.contains("line 40"));
    assert!(!app.scroll.is_following());
    // The status bar says the view is held back, so silence is not read as a stalled agent.
    assert!(scrolled.contains("scrolled"));

    // More output arrives while the reader is scrolled up.
    let more: Vec<_> = (41..=50).map(|n| message(n, &format!("line {n}"))).collect();
    app.ledger.apply(1, &JournalItem::delta(JournalChange::Append, more));
    app.measure(20);
    let after = render_to_text(&app, 100, 20);
    // The same region stays on screen rather than being yanked to the new tail.
    assert_eq!(
        scrolled.contains("line 20"),
        after.contains("line 20"),
        "the reader's position moved"
    );
    assert!(!after.contains("line 50"));
}

#[test]
fn search_reports_its_matches_and_reveals_them() {
    use dsh_tui_proto::{HistoryRecord, JournalChange, JournalItem, SessionEvent};

    let records: Vec<_> = (1..=40)
        .map(|n| HistoryRecord::Event {
            event: SessionEvent {
                kind: "assistant/message".into(),
                seq: n,
                time: Some(0),
                data: serde_json::json!({ "content": [{ "type": "text",
                    "text": if n == 3 { "the tokenizer bound".to_string() }
                            else { format!("line {n}") } }] }),
            },
        })
        .collect();

    let mut app = ready_app();
    app.ledger
        .apply(1, &JournalItem::delta(JournalChange::Replace, records));
    app.measure(20);

    app.open_search();
    for ch in "tokenizer".chars() {
        app.edit_search(dsh_tui::app::SearchEdit::Push(ch));
    }
    assert_eq!(app.search_hits.len(), 1);

    app.measure(20);
    let text = render_to_text(&app, 100, 20);
    println!("{text}");
    assert!(text.contains("search tokenizer"));
    // The hint must name keys that actually navigate; plain letters type into the query.
    assert!(text.contains("^n next"));
    assert!(!text.contains(" n next"));
    assert!(text.contains("1/1"));
    // The match was off-screen at the tail; revealing it scrolled the view to include it.
    assert!(text.contains("the tokenizer bound"));

    // A query with no matches says so rather than showing an empty count.
    for _ in 0..9 {
        app.edit_search(dsh_tui::app::SearchEdit::Backspace);
    }
    for ch in "absent".chars() {
        app.edit_search(dsh_tui::app::SearchEdit::Push(ch));
    }
    app.measure(20);
    assert!(render_to_text(&app, 100, 20).contains("no matches"));
}

#[test]
fn nested_dispatches_render_under_their_program() {
    use dsh_tui_proto::{HistoryRecord, JournalChange, JournalItem, SessionEvent};

    fn event(kind: &str, seq: u64, data: serde_json::Value) -> HistoryRecord {
        HistoryRecord::Event {
            event: SessionEvent { kind: kind.into(), seq, time: Some(0), data },
        }
    }

    let mut app = ready_app();
    app.ledger.apply(
        1,
        &JournalItem::delta(
            JournalChange::Replace,
            vec![
                event("tool/call", 1, serde_json::json!({
                    "turn": 1, "step": 0, "callId": "root1", "name": "run_code",
                    "arguments": r#"{"code":"await bash(\"ls\")"}"# })),
                event("tool/code-dispatch-start", 2, serde_json::json!({
                    "rootCallId": "root1", "parentCallId": "root1",
                    "subCallId": "root1:code:1", "name": "bash",
                    "arguments": { "command": "ls -la" } })),
                event("tool/code-dispatch", 3, serde_json::json!({
                    "rootCallId": "root1", "parentCallId": "root1",
                    "subCallId": "root1:code:1", "name": "bash", "arguments": {},
                    "isError": false, "content": [] })),
                event("tool/code-dispatch-start", 4, serde_json::json!({
                    "rootCallId": "root1", "parentCallId": "root1:code:1",
                    "subCallId": "root1:code:2", "name": "read_file",
                    "arguments": { "path": "src/lex.rs" } })),
            ],
        ),
    );
    app.measure(20);

    let text = render_to_text(&app, 100, 20);
    println!("{text}");
    assert!(text.contains("run_code"));
    assert!(text.contains("bash  ls -la"));
    assert!(text.contains("read_file  src/lex.rs"));
    // The deeper dispatch is indented further than its parent.
    let bash_indent = text.lines().find(|l| l.contains("bash  ls -la")).unwrap()
        .find('$').unwrap_or(0);
    let read_indent = text.lines().find(|l| l.contains("read_file")).unwrap()
        .find('▤').unwrap_or(0);
    assert!(read_indent > bash_indent, "nested dispatch should indent further");
    // The unsettled one reads as running.
    assert!(text.contains("running"));
}

#[test]
fn a_terminal_without_an_image_protocol_names_the_reason() {
    let mut app = ready_app();
    app.focus = dsh_tui::app::Pane::Conversation;
    app.graphics = dsh_tui::attachment::Graphics::None;
    app.drafts.push(dsh_tui::attachment::Draft {
        name: "shot.png".into(),
        media_type: "image/png".into(),
        data: vec![0u8; 2048],
        dimensions: Some((100, 100)),
    });
    app.measure(20);

    let text = render_to_text(&app, 110, 20);
    println!("{text}");
    // The placeholder names the file and why it cannot be drawn.
    assert!(text.contains("shot.png"));
    assert!(text.contains("cannot show images inline"));
    // Nothing was reserved, since nothing will be written over it.
    let (placements, unsupported) = app.image_placements(110, 20);
    assert!(placements.is_empty());
    assert_eq!(unsupported.len(), 1);
}

#[test]
fn a_kitty_terminal_reserves_cells_and_produces_a_payload() {
    let mut app = ready_app();
    app.focus = dsh_tui::app::Pane::Conversation;
    app.graphics = dsh_tui::attachment::Graphics::Kitty;
    app.drafts.push(dsh_tui::attachment::Draft {
        name: "shot.png".into(),
        media_type: "image/png".into(),
        data: vec![0x89, b'P', b'N', b'G', 0, 0, 0, 0],
        dimensions: Some((100, 100)),
    });

    let (placements, unsupported) = app.image_placements(110, 20);
    assert_eq!(placements.len(), 1);
    assert!(unsupported.is_empty());
    // Positioned over the reserved row, inside the centre pane.
    assert!(placements[0].payload.starts_with("\x1b["));
    assert!(placements[0].payload.contains("a=T,f=100"));
    assert!(placements[0].rect.y < 20);
}

#[test]
fn a_jpeg_on_kitty_falls_back_rather_than_sending_bytes_it_rejects() {
    let mut app = ready_app();
    app.graphics = dsh_tui::attachment::Graphics::Kitty;
    app.drafts.push(dsh_tui::attachment::Draft {
        name: "photo.jpg".into(),
        media_type: "image/jpeg".into(),
        data: vec![0xff, 0xd8, 0xff],
        dimensions: None,
    });

    let (placements, unsupported) = app.image_placements(110, 20);
    // kitty's f=100 is PNG; sending a JPEG under it would be rejected by the terminal.
    assert!(placements.is_empty());
    assert!(unsupported[0].contains("cannot take this format"));
}

#[test]
fn every_settings_page_title_translates() {
    for (section, english, chinese) in [
        (dsh_tui::app::SettingsSection::Namespaces, "Namespaces", "命名空间"),
        (dsh_tui::app::SettingsSection::Models, "Models", "模型"),
        (dsh_tui::app::SettingsSection::Plugins, "Plugins", "插件"),
        (dsh_tui::app::SettingsSection::General, "General", "通用"),
    ] {
        let mut app = ready_app();
        app.view = dsh_tui::app::View::Settings;
        app.settings_section = section;
        let text = render_to_text(&app, 100, 14);
        assert!(text.contains(english), "english {english}");

        app.locale = dsh_tui::locale::Locale::Zh;
        let text = render_to_text(&app, 100, 14);
        assert!(text.contains(chinese), "chinese {chinese}");
        assert!(text.contains("设置"), "the Settings base should translate too");
    }
}

#[test]
fn the_ui_carries_no_untranslated_english_prose() {
    // Every user-facing sentence should reach the dictionaries; a literal left behind
    // renders English in a Chinese session.
    let source = std::fs::read_to_string(format!("{}/src/ui.rs", env!("CARGO_MANIFEST_DIR")))
        .expect("ui source");
    let mut stragglers = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with("///") {
            continue;
        }
        if line.contains("t_or(") {
            continue;
        }
        for literal in line.split('"').skip(1).step_by(2) {
            // A sentence: several words, starting with a capital.
            let words = literal.split_whitespace().count();
            let leads_upper = literal.chars().next().is_some_and(char::is_uppercase);
            if words >= 3 && leads_upper && !literal.contains('{') {
                stragglers.push(literal.to_string());
            }
        }
    }
    assert!(
        stragglers.is_empty(),
        "untranslated prose in ui.rs: {stragglers:?}"
    );
}

#[test]
fn the_sidebar_names_the_workspace_and_lists_only_its_sessions() {
    // The workspace is the scope of everything under it in this pane. Without it here,
    // choosing one changed the session list with no visible reason.
    let mut app = ready_app();
    app.workspaces.apply(
        1,
        serde_json::from_value(serde_json::json!({
            "type": "baseline",
            "value": {
                "items": [{
                    "workspaceId": "w-1", "path": "/home/acp/projects/dsh-tui",
                    "title": "dsh-tui", "sessionIds": ["s-1"],
                    "createdAt": "", "updatedAt": ""
                }],
                "archivedSessionIds": []
            }
        }))
        .expect("baseline"),
    );
    app.workspace = Some("/home/acp/projects/dsh-tui".into());

    let text = render_to_text(&app, 100, 20);
    println!("{text}");

    // Named, with its path.
    assert!(text.contains("dsh-tui"), "the workspace should be named");
    assert!(text.contains("/projects/dsh-tui"), "and its path shown");
    // Scoped: s-1 belongs to it, s-2 does not.
    assert!(text.contains("Wire the trajectory pane"));
    assert!(
        !text.contains("Port the approval modal"),
        "a session outside the workspace must not be listed:\n{text}"
    );
    // The brand attribution survives the new header.
    assert!(text.contains("built on DSH"));
}

#[test]
fn the_sidebar_says_when_there_is_no_workspace() {
    let app = ready_app();
    let text = render_to_text(&app, 100, 20);
    assert!(text.contains("no workspace"), "{text}");
    // With no workspace nothing is scoped, so every session stays listed.
    assert!(text.contains("Wire the trajectory pane"));
    assert!(text.contains("Port the approval modal"));
}

/// A listing with `count` subdirectories, more than any popup can show at once.
fn crowded_picker(count: usize) -> dsh_tui::directory::Browser {
    let entries: Vec<serde_json::Value> = (0..count)
        .map(|i| {
            serde_json::json!({
                "name": format!("folder-{i:02}"),
                "path": format!("/home/acp/projects/folder-{i:02}"),
                "hidden": false
            })
        })
        .collect();
    let mut browser = dsh_tui::directory::Browser::new();
    browser.replace(
        serde_json::from_value(serde_json::json!({
            "path": "/home/acp/projects",
            "home": "/home/acp",
            "crumbs": [
                { "name": "/", "path": "/", "hidden": false },
                { "name": "acp", "path": "/home/acp", "hidden": false },
                { "name": "projects", "path": "/home/acp/projects", "hidden": false }
            ],
            "entries": entries,
            "truncated": false
        }))
        .expect("listing"),
    );
    browser
}

#[test]
fn the_picker_scrolls_to_keep_the_cursor_visible() {
    // Drawn as a flat paragraph the rows past the popup's height were clipped, so the
    // cursor walked off the bottom into folders that could never be seen or reached.
    let mut app = ready_app();
    let mut browser = crowded_picker(40);
    for _ in 0..35 {
        browser.select_next();
    }
    assert_eq!(browser.selected, 35);
    app.picker = Some(browser);

    let text = render_to_text(&app, 100, 24);
    println!("{text}");

    // The highlighted folder is on screen...
    assert!(
        text.contains("folder-34"),
        "the cursor's row must be visible:\n{text}"
    );
    // ...and the top of the list has scrolled away to make room.
    assert!(
        !text.contains("folder-00"),
        "the window should have scrolled past the first rows:\n{text}"
    );
    // The footer stays put rather than scrolling off with the entries.
    assert!(text.contains("enter select"), "{text}");
}

#[test]
fn the_picker_shows_the_top_of_the_list_before_anything_moves() {
    let mut app = ready_app();
    app.picker = Some(crowded_picker(40));
    let text = render_to_text(&app, 100, 24);
    assert!(text.contains("use this directory"), "{text}");
    assert!(text.contains("folder-00"), "{text}");
    assert!(!text.contains("folder-39"), "{text}");
}

#[test]
fn paging_reaches_the_end_of_a_long_listing_and_stops_there() {
    let mut browser = crowded_picker(40);
    // 41 cursor positions: the synthetic row plus 40 entries.
    browser.select_last();
    assert_eq!(browser.selected, 40);
    assert_eq!(browser.chosen_path(), Some("/home/acp/projects/folder-39"));

    browser.select_first();
    assert!(browser.on_use_row());

    // Paging is clamped at both ends rather than wrapping or running past.
    for _ in 0..20 {
        browser.page_down(10);
    }
    assert_eq!(browser.selected, 40);
    for _ in 0..20 {
        browser.page_up(10);
    }
    assert_eq!(browser.selected, 0);
}

#[test]
fn a_fresh_workspace_reads_as_ready_rather_than_empty() {
    // "Nothing here yet" plus a live composer, not a bare "No messages yet." over a
    // transcript that belongs to somewhere else.
    let mut app = ready_app();
    app.workspaces.apply(
        1,
        serde_json::from_value(serde_json::json!({
            "type": "baseline",
            "value": {
                "items": [
                    { "workspaceId": "w-1", "path": "/home/acp/old", "title": "old",
                      "sessionIds": ["s-1", "s-2"], "createdAt": "", "updatedAt": "" },
                    { "workspaceId": "w-2", "path": "/home/acp/fresh", "title": "fresh",
                      "sessionIds": [], "createdAt": "", "updatedAt": "" }
                ],
                "archivedSessionIds": []
            }
        }))
        .expect("baseline"),
    );
    app.workspace = Some("/home/acp/fresh".into());

    let text = render_to_text(&app, 100, 20);
    println!("{text}");

    assert!(text.contains("Nothing here yet"), "{text}");
    assert!(text.contains("Type below to start"), "{text}");
    // The composer is on screen, so there is somewhere to start.
    assert!(text.contains("Ask anything"), "{text}");
    // And nothing from the workspace with sessions leaks in.
    assert!(!text.contains("Wire the trajectory pane"), "{text}");
    assert!(!text.contains("Port the approval modal"), "{text}");
}

#[test]
fn an_unscoped_session_list_names_each_session_workspace() {
    // With no workspace chosen the list holds sessions from everywhere, and nothing on
    // the row would otherwise say which one a session belongs to.
    let mut app = ready_app();
    app.sessions[0].cwd = Some("/home/acp/alpha".into());
    app.sessions[1].cwd = Some("/home/acp/beta".into());
    app.workspaces.apply(
        1,
        serde_json::from_value(serde_json::json!({
            "type": "baseline",
            "value": {
                "items": [
                    { "workspaceId": "w-1", "path": "/home/acp/alpha", "title": "alpha",
                      "sessionIds": [], "createdAt": "", "updatedAt": "" },
                    { "workspaceId": "w-2", "path": "/home/acp/beta", "title": "beta",
                      "sessionIds": [], "createdAt": "", "updatedAt": "" }
                ],
                "archivedSessionIds": []
            }
        }))
        .expect("baseline"),
    );
    assert!(app.workspace.is_none(), "no workspace chosen");

    let text = render_to_text(&app, 100, 20);
    println!("{text}");
    assert!(text.contains("alpha"), "the first session's workspace: {text}");
    assert!(text.contains("beta"), "the second session's workspace: {text}");
}

#[test]
fn the_sidebar_names_the_session_that_is_open() {
    // The cursor and the open session are different things now: scrolling browses.
    let mut app = ready_app();
    app.workspaces.apply(
        1,
        serde_json::from_value(serde_json::json!({
            "type": "baseline",
            "value": {
                "items": [{ "workspaceId": "w-1", "path": "/home/acp/alpha", "title": "alpha",
                            "sessionIds": ["s-1", "s-2"], "createdAt": "", "updatedAt": "" }],
                "archivedSessionIds": []
            }
        }))
        .expect("baseline"),
    );
    app.workspace = Some("/home/acp/alpha".into());

    // Nothing open yet.
    let text = render_to_text(&app, 100, 20);
    assert!(text.contains("no session open"), "{text}");

    // Once one is open the header names it, and the rows drop the workspace tag because
    // a scoped list cannot be mixed.
    app.set_followed_session_for_test(Some("s-2".into()));
    let text = render_to_text(&app, 100, 20);
    println!("{text}");
    assert!(text.contains("▸ Port the approval modal"), "{text}");
    assert!(!text.contains("no session open"), "{text}");
    // A list already scoped to one workspace does not repeat it on every row.
    assert!(!text.contains("    alpha"), "the tag is noise here:\n{text}");
}
