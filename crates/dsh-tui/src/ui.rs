//! Rendering: the three-column `AppFrame` the web client's `ui-layout` defines, plus the
//! modal seat that blocking waterfalls occupy.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use ratatui::Frame;

use crate::app::{App, Connection, Pane, SettingsSection, View};
use crate::calltree;
use crate::chips::PlanChip;
use crate::locale::t_or;
use crate::logging::{self, Level, Source};
use crate::session::{Row, RowKind};
use crate::trajectory;
use crate::plugins::Health;
use crate::settings::Editor;

pub fn render(frame: &mut Frame, app: &App) {
    let theme = app.theme;
    let area = frame.area();

    frame.render_widget(
        Block::default().style(Style::default().bg(theme.bg_base).fg(theme.text)),
        area,
    );

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);

    let columns = app_frame(rows[0], app);
    let mut column = columns.into_iter();

    if app.sidebar_open {
        render_sidebar(frame, column.next().unwrap(), app);
    }
    let centre = column.next().unwrap();
    match app.view {
        View::Conversation => render_conversation(frame, centre, app),
        View::Settings => render_settings(frame, centre, app),
        View::Workspace => render_workspace(frame, centre, app),
        View::Logs => render_logs(frame, centre, app),
    }
    if app.details_open {
        render_details(frame, column.next().unwrap(), app);
    }

    render_status(frame, rows[1], app);

    // A question request takes over the composer; the agent is blocked until it settles.
    if let Some(pending) = app.questions.as_ref() {
        render_questions(frame, area, app, pending);
    }

    if let Some(picker) = app.picker.as_ref() {
        render_directory_picker(frame, area, app, picker);
    }

    // A blocked waterfall owns the screen: the agent cannot proceed until it is answered.
    if let Some(ask) = app.asks.front() {
        render_ask_modal(frame, area, app, ask);
    }
}

/// Split the body into the visible columns, mirroring the web frame's concession behavior:
/// the conversation keeps the remaining width and the side columns hold fixed seats.
fn app_frame(area: Rect, app: &App) -> Vec<Rect> {
    let mut constraints = Vec::new();
    if app.sidebar_open {
        constraints.push(Constraint::Length(30));
    }
    constraints.push(Constraint::Min(40));
    if app.details_open {
        constraints.push(Constraint::Length(44));
    }
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area)
        .to_vec()
}

fn pane_block<'a>(title: &'a str, focused: bool, app: &App) -> Block<'a> {
    Block::default()
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(if focused { app.theme.accent } else { app.theme.text_dim })
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(app.theme.pane_border(focused)))
        .style(Style::default().bg(app.theme.bg_layer))
}

fn render_sidebar(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Pane::Sidebar;
    let block = pane_block(t_or(app.locale, "sidebar.sessions", "Sessions"), focused, app);
    let outer = block.inner(area);
    frame.render_widget(block, area);

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Length(4), Constraint::Min(1)])
        .split(outer);

    // The brand row. Per the DSH brand guidelines this names the harness the TUI is built
    // on without adopting its trademark as this project's own name.
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "dsh-tui",
                Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "built on DSH",
                Style::default().fg(app.theme.text_dim),
            )),
        ]),
        split[0],
    );

    render_workspace_header(frame, split[1], app);
    let inner = split[2];

    if app.sessions.is_empty() {
        let hint = if app.workspace.is_none() {
            t_or(app.locale, "sidebar.emptyNoWorkspace", "No sessions yet — press n to choose a directory")
        } else {
            t_or(app.locale, "sidebar.empty", "No sessions yet — press n for a new one")
        };
        let empty = Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(app.theme.text_dim),
        )))
        .wrap(Wrap { trim: true });
        frame.render_widget(empty, inner);
        return;
    }

    let visible = app.visible_sessions();
    let items: Vec<ListItem> = visible
        .iter()
        .filter_map(|index| app.sessions.get(*index).map(|row| (*index, row)))
        .map(|(index, row)| {
            let selected = index == app.selected_session;
            let marker = if row.running { "●" } else { "○" };
            let marker_style = Style::default().fg(if row.running {
                app.theme.success
            } else {
                app.theme.text_dim
            });
            let title_style = if selected {
                Style::default()
                    .fg(app.theme.text)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.text_dim)
            };
            let mut spans = vec![
                Span::styled(format!("{marker} "), marker_style),
                Span::styled(row.title.clone(), title_style),
            ];
            // A subagent session is listed but is not a top-level conversation.
            if row.is_subagent {
                spans.push(Span::styled(
                    "  sub",
                    Style::default().fg(app.theme.text_dim),
                ));
            }
            // Name the workspace only when the row does not belong to the current one —
            // with none chosen the list holds sessions from everywhere, and in a fallback
            // list the foreign rows are exactly the surprising ones. Repeating the
            // current workspace on every row of a list already scoped to it is noise.
            let mut lines = vec![Line::from(spans)];
            let owner = app.workspace_of(row);
            let foreign = match (&owner, app.workspace.as_deref()) {
                (Some(owner), Some(current)) => owner.path != current,
                (Some(_), None) => true,
                (None, _) => false,
            };
            if foreign {
                if let Some(owner) = owner {
                    // Its own line: appended to the title it is clipped off the end of a
                    // sidebar this narrow, which is worse than not showing it at all.
                    lines.push(Line::from(Span::styled(
                        format!("    {}", owner.title),
                        Style::default().fg(app.theme.accent),
                    )));
                }
            }
            ListItem::new(lines)
        })
        .collect();

    // Stateful, so a long session list scrolls with the cursor instead of clipping it.
    let mut state = ListState::default();
    state.select(
        visible
            .iter()
            .position(|index| *index == app.selected_session),
    );
    frame.render_stateful_widget(List::new(items), inner, &mut state);
}

/// The workspace the sessions below are scoped to.
///
/// It sits in the sidebar rather than only in the status bar because it is the scope of
/// everything under it: without it here, choosing a workspace changes the list with no
/// visible reason.
fn render_workspace_header(frame: &mut Frame, area: Rect, app: &App) {
    let lines = match app.workspace_label() {
        Some(path) => {
            // The host's title when it has one, so the header reads the way `^w` does.
            let name = app
                .workspaces
                .rows()
                .into_iter()
                .find(|row| Some(row.path.as_str()) == app.workspace.as_deref())
                .map(|row| row.title)
                .unwrap_or_else(|| {
                    path.rsplit('/').next().unwrap_or(path.as_str()).to_string()
                });
            let scoped = app.sessions_are_scoped();
            vec![
                Line::from(vec![
                    Span::styled(
                        format!("▪ {name}"),
                        Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD),
                    ),
                    // Only worth saying when the list is actually hiding something.
                    Span::styled(
                        if scoped {
                            format!("  {}", app.visible_sessions().len())
                        } else {
                            String::new()
                        },
                        Style::default().fg(app.theme.text_dim),
                    ),
                ]),
                Line::from(Span::styled(
                    format!("  {path}"),
                    Style::default().fg(app.theme.text_dim),
                )),
                // Which session is actually open. The cursor can be anywhere else in the
                // list, so without this there is nothing on screen naming the transcript.
                match app.open_session() {
                    Some(session) => Line::from(vec![
                        Span::styled(
                            "  ▸ ",
                            Style::default().fg(app.theme.success),
                        ),
                        Span::styled(
                            session.title.clone(),
                            Style::default().fg(app.theme.text),
                        ),
                    ]),
                    None => Line::from(Span::styled(
                        t_or(app.locale, "sidebar.noSession", "  no session open"),
                        Style::default().fg(app.theme.text_dim),
                    )),
                },
            ]
        }
        None => vec![Line::from(Span::styled(
            t_or(app.locale, "sidebar.noWorkspace", "▪ no workspace"),
            Style::default().fg(app.theme.warning),
        ))],
    };
    frame.render_widget(Paragraph::new(lines), area);
}

fn render_conversation(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Pane::Conversation;
    let block = pane_block(t_or(app.locale, "conversation.title", "Conversation"), focused, app);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1), Constraint::Length(3)])
        .split(inner);

    let body = match &app.connection {
        Connection::Connecting => vec![Line::from(Span::styled(
            t_or(app.locale, "conversation.starting", "Starting the harness runtime…"),
            Style::default().fg(app.theme.text_dim),
        ))],
        Connection::Failed(reason) => vec![
            Line::from(Span::styled(
                t_or(app.locale, "conversation.disconnected", "Disconnected"),
                Style::default().fg(app.theme.danger).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                reason.clone(),
                Style::default().fg(app.theme.text_dim),
            )),
        ],
        // A workspace with no sessions yet is not "no messages" — it is a fresh start,
        // and the composer below is already live: typing creates the session.
        Connection::Ready(_) if app.workspace_is_empty() => vec![
            Line::from(Span::styled(
                t_or(app.locale, "conversation.freshWorkspace", "Nothing here yet."),
                Style::default().fg(app.theme.text).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                t_or(app.locale, "conversation.freshHint", "Type below to start the first session."),
                Style::default().fg(app.theme.text_dim),
            )),
        ],
        Connection::Ready(_) if app.ledger.is_empty() => vec![Line::from(Span::styled(
            t_or(app.locale, "conversation.empty", "No messages yet."),
            Style::default().fg(app.theme.text_dim),
        ))],
        Connection::Ready(_) => conversation_lines(app, split[0].height),
    };

    frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), split[0]);
    if let Some(query) = app.search.as_ref() {
        render_search_bar(frame, split[1], app, query);
    } else {
        render_chips(frame, split[1], app);
    }
    render_composer(frame, split[2], app, focused);
    if focused {
        if app.model_picker.is_some() {
            render_model_picker(frame, split[2], app);
        } else {
            render_candidates(frame, split[2], app);
        }
    }
}

fn render_composer(frame: &mut Frame, area: Rect, app: &App, focused: bool) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(app.theme.pane_border(focused)));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let content = if app.composer.is_empty() {
        Line::from(Span::styled(
            t_or(app.locale, "composer.placeholder", "Ask anything, / for commands, @ for references"),
            Style::default().fg(app.theme.text_dim),
        ))
    } else {
        Line::from(vec![
            Span::styled("› ", Style::default().fg(app.theme.accent)),
            Span::styled(
                app.composer.text().to_string(),
                Style::default().fg(app.theme.text),
            ),
        ])
    };
    frame.render_widget(Paragraph::new(content), inner);

    // Park the terminal cursor at the caret so the composer behaves like a real input.
    if focused && !app.composer.is_empty() {
        let prefix = app.composer.text()[..app.composer.caret()].chars().count() as u16;
        let x = inner.x.saturating_add(2).saturating_add(prefix).min(inner.right().saturating_sub(1));
        frame.set_cursor_position((x, inner.y));
    }
}

/// The conversation search bar, replacing the chip strip while it is open.
fn render_search_bar(frame: &mut Frame, area: Rect, app: &App, query: &str) {
    let position = if app.search_hits.is_empty() {
        if query.trim().is_empty() {
            String::new()
        } else {
            "  no matches".to_string()
        }
    } else {
        format!("  {}/{}", app.search_index + 1, app.search_hits.len())
    };
    let line = Line::from(vec![
        Span::styled("search ", Style::default().fg(app.theme.text_dim)),
        Span::styled(
            query.to_string(),
            Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            position,
            Style::default().fg(if app.search_hits.is_empty() && !query.trim().is_empty() {
                app.theme.warning
            } else {
                app.theme.text_dim
            }),
        ),
        Span::styled(
            "   enter/^n next · ^p previous · esc close",
            Style::default().fg(app.theme.text_dim),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// The composer's context strip: goal, plan mode, jobs, and the model seat.
fn render_chips(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans: Vec<Span> = Vec::new();

    if let Some(goal) = app.goal() {
        let color = match goal.phase.as_str() {
            "blocked" => app.theme.danger,
            "complete" => app.theme.success,
            "paused" => app.theme.warning,
            _ => app.theme.accent,
        };
        spans.push(Span::styled(goal.summary(), Style::default().fg(color)));
        // An active goal a disarmed process will not continue must not look like one
        // that is about to proceed on its own.
        if goal.will_continue() == Some(false) && goal.phase == "active" {
            spans.push(Span::styled(
                " (disarmed)",
                Style::default().fg(app.theme.warning),
            ));
        }
        spans.push(Span::styled("   ", Style::default()));
    }

    // An absent plan capability takes no seat at all, rather than reading as "off".
    let plan = app.plan_chip();
    if plan.is_visible() {
        let color = match plan {
            PlanChip::On => app.theme.accent,
            PlanChip::Pending => app.theme.warning,
            _ => app.theme.text_dim,
        };
        spans.push(Span::styled(plan.label(), Style::default().fg(color)));
        spans.push(Span::styled("   ", Style::default()));
    }

    // Absent when no permission service is composed; the control is hidden, not empty.
    if let Some(permissions) = app.permissions() {
        let color = if permissions.is_custom() {
            app.theme.warning
        } else {
            app.theme.text_dim
        };
        spans.push(Span::styled(
            format!("{}   ", permissions.current_label()),
            Style::default().fg(color),
        ));
    }

    // Staged images. Those the terminal can draw get cells reserved for them here and the
    // escape sequence written after the frame; the rest name why they cannot be shown.
    if !app.drafts.is_empty() {
        let (placements, unsupported) = app.image_placements(area.width + area.x + 2, area.y + 5);
        if !placements.is_empty() {
            // Blank cells hold the space the image is written over.
            spans.push(Span::styled(
                " ".repeat(placements.len() * 4),
                Style::default(),
            ));
        }
        for text in unsupported.iter().take(2) {
            spans.push(Span::styled(
                format!("{text}   "),
                Style::default().fg(app.theme.text_dim),
            ));
        }
    }
    if let Some(error) = app.attachment_error.as_ref() {
        spans.push(Span::styled(
            format!("{error}   "),
            Style::default().fg(app.theme.danger),
        ));
    }
    // A prompt that did not go out. The composer empties on submit, so without this the
    // only evidence of a rejected message is that nothing happened.
    if let Some(error) = app.prompt_error.as_ref() {
        spans.push(Span::styled(
            format!("not sent: {error}   "),
            Style::default().fg(app.theme.danger).add_modifier(Modifier::BOLD),
        ));
    }

    let jobs = app.live_jobs();
    if jobs > 0 {
        spans.push(Span::styled(
            format!("{jobs} job{}   ", if jobs == 1 { "" } else { "s" }),
            Style::default().fg(app.theme.warning),
        ));
    }

    spans.push(Span::styled(
        "^p model",
        Style::default().fg(app.theme.text_dim),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The `/model` picker, floating above the composer.
fn render_model_picker(frame: &mut Frame, composer_area: Rect, app: &App) {
    let choices = app.model_choices();
    let notices = app.catalog.failures.len() + app.catalog.empty_providers.len();
    let rows = (choices.len() as u16).min(7) + notices.min(3) as u16;
    let height = (rows + 2).max(3);
    let y = composer_area.y.saturating_sub(height);
    let area = Rect {
        x: composer_area.x,
        y,
        width: composer_area.width,
        height: height.min(composer_area.y.max(1)),
    };
    if area.height < 3 {
        return;
    }

    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(Span::styled(
            " model ",
            Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(app.theme.accent))
        .style(Style::default().bg(app.theme.bg_layer));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    for (index, choice) in choices.iter().enumerate() {
        let selected = index == app.model_row;
        let style = if selected {
            Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.theme.text)
        };
        lines.push(Line::from(vec![
            Span::styled(if selected { "▸ " } else { "  " }, style),
            Span::styled(choice.label(), style),
        ]));
    }
    // A provider that failed or listed nothing is named, so its absence from the list is
    // explained rather than read as "not configured".
    for failure in app.catalog.failures.iter().take(2) {
        lines.push(Line::from(Span::styled(
            format!("  {} unavailable: {}", failure.name, failure.message),
            Style::default().fg(app.theme.danger),
        )));
    }
    for provider in app.catalog.empty_providers.iter().take(1) {
        lines.push(Line::from(Span::styled(
            format!("  {provider} listed no models"),
            Style::default().fg(app.theme.warning),
        )));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            t_or(app.locale, "models.catalogReading", "Reading the model catalog…"),
            Style::default().fg(app.theme.text_dim),
        )));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The `/` and `@` candidate menu, floating directly above the composer.
fn render_candidates(frame: &mut Frame, composer_area: Rect, app: &App) {
    let candidates = app.visible_candidates();
    if candidates.is_empty() {
        return;
    }

    let rows = (candidates.len() as u16).min(8);
    let height = rows + 2;
    // Anchor to the composer and grow upward; a menu that grows down would fall off-screen.
    let y = composer_area.y.saturating_sub(height);
    let area = Rect {
        x: composer_area.x,
        y,
        width: composer_area.width,
        height: height.min(composer_area.y.max(1)),
    };
    if area.height < 3 {
        return;
    }

    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(app.theme.accent))
        .style(Style::default().bg(app.theme.bg_layer));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let items: Vec<ListItem> = candidates
        .iter()
        .take(inner.height as usize)
        .enumerate()
        .map(|(index, candidate)| {
            let selected = index == app.candidate_index;
            let label_style = if selected {
                Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.text)
            };
            ListItem::new(Line::from(vec![
                Span::styled(if selected { "▸ " } else { "  " }, label_style),
                Span::styled(candidate.label.clone(), label_style),
                Span::styled(
                    if candidate.detail.is_empty() {
                        String::new()
                    } else {
                        format!("  {}", candidate.detail)
                    },
                    Style::default().fg(app.theme.text_dim),
                ),
            ]))
        })
        .collect();
    frame.render_widget(List::new(items), inner);
}

/// Project ledger rows into styled lines, keeping the tail visible.
///
/// Each row gets a speaker gutter rather than a bare block of text, so a long streamed
/// answer stays distinguishable from the tool calls around it.
fn conversation_lines(app: &App, height: u16) -> Vec<Line<'static>> {
    let theme = app.theme;
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Sub-dispatches nest under the `run_code` call that issued them, so the tool rows
    // come from the call tree while everything else comes from the flat ledger.
    let tree = calltree::rows(&calltree::build(&app.ledger));
    let mut tool_rows = tree.iter().filter(|row| row.depth > 0 || row.detached).peekable();

    for row in app.ledger.rows() {
        let style = row_style(&row, &theme);
        for (index, text) in row.text.lines().enumerate() {
            // The gutter marks the first line only; continuations align under it.
            let prefix = if index == 0 { row.glyph } else { " " };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{prefix} "),
                    Style::default().fg(gutter_color(&row, &theme)),
                ),
                Span::styled(text.to_string(), style),
            ]));
        }

        // A tool call's nested dispatches follow it, indented.
        if row.kind == RowKind::ToolCall {
            while let Some(nested) = tool_rows.peek() {
                if nested.depth == 0 && !nested.detached {
                    break;
                }
                let nested = tool_rows.next().unwrap();
                lines.push(nested_line(nested, &theme));
                if tool_rows.peek().is_some_and(|next| next.depth == 0) {
                    break;
                }
            }
        }
        lines.push(Line::from(""));
    }

    // Sub-calls whose parent is outside the loaded window have no row to follow.
    for orphan in tool_rows.filter(|row| row.detached) {
        lines.push(nested_line(orphan, &theme));
    }

    // The viewport measures from the bottom, so appended lines never move a reader who
    // has scrolled up; `window` returns the slice to draw.
    let (start, end) = if app.scroll.is_measured() {
        app.scroll.window(lines.len())
    } else {
        // No layout pass yet: fall back to the tail, which is what a fresh view shows.
        (lines.len().saturating_sub(height as usize), lines.len())
    };
    let current = app.current_match();
    lines
        .into_iter()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(|(index, line)| {
            if Some(index) == current {
                // The current match is marked in place rather than by jumping the view.
                line.style(Style::default().bg(theme.bg_layer).fg(theme.accent))
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .into_iter()
        .take(height as usize)
        .collect()
}

/// Plain text of every conversation line, for search and scroll measurement.
pub fn conversation_text(app: &App) -> Vec<String> {
    // Must produce one entry per rendered line: the viewport's offsets and the search's
    // match indices both address this list.
    let tree = calltree::rows(&calltree::build(&app.ledger));
    let mut tool_rows = tree.iter().filter(|row| row.depth > 0 || row.detached).peekable();

    let mut lines = Vec::new();
    for row in app.ledger.rows() {
        for text in row.text.lines() {
            lines.push(text.to_string());
        }
        if row.kind == RowKind::ToolCall {
            while let Some(nested) = tool_rows.peek() {
                if nested.depth == 0 && !nested.detached {
                    break;
                }
                let nested = tool_rows.next().unwrap();
                lines.push(format!("{} {}", nested.label, nested.status));
                if tool_rows.peek().is_some_and(|next| next.depth == 0) {
                    break;
                }
            }
        }
        lines.push(String::new());
    }
    for orphan in tool_rows.filter(|row| row.detached) {
        lines.push(format!("{} {}", orphan.label, orphan.status));
    }
    lines
}

/// One nested sub-dispatch line, indented by depth.
fn nested_line(row: &calltree::Row, theme: &crate::theme::Theme) -> Line<'static> {
    let color = if row.failed {
        theme.danger
    } else if row.status == "running" {
        theme.warning
    } else {
        theme.text_dim
    };
    let mut spans = vec![
        Span::styled(
            format!("{}{} ", "  ".repeat(row.depth.max(1)), row.glyph),
            Style::default().fg(color),
        ),
        Span::styled(row.label.clone(), Style::default().fg(theme.text_dim)),
        Span::styled(format!("  {}", row.status), Style::default().fg(color)),
    ];
    if row.detached {
        // Its parent is older than the loaded window, not missing.
        spans.push(Span::styled(
            "  parent not loaded",
            Style::default().fg(theme.text_dim),
        ));
    }
    Line::from(spans)
}

fn row_style(row: &Row, theme: &crate::theme::Theme) -> Style {
    if row.failed {
        return Style::default().fg(theme.danger);
    }
    match row.kind {
        RowKind::User => Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        RowKind::Assistant => Style::default().fg(theme.text),
        RowKind::Reasoning => Style::default()
            .fg(theme.text_dim)
            .add_modifier(Modifier::ITALIC),
        RowKind::ToolCall => Style::default().fg(theme.text),
        RowKind::ToolResult => Style::default().fg(theme.text_dim),
        RowKind::TurnBoundary | RowKind::Other => Style::default().fg(theme.text_dim),
    }
}

fn gutter_color(row: &Row, theme: &crate::theme::Theme) -> ratatui::style::Color {
    if row.failed {
        return theme.danger;
    }
    match row.kind {
        RowKind::User | RowKind::ToolCall => theme.accent,
        RowKind::ToolResult => theme.success,
        _ => theme.text_dim,
    }
}

/// The settings surface: the generic namespace form, or the Models page.
fn render_settings(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Pane::Conversation;
    let section = match app.settings_section {
        SettingsSection::Namespaces => t_or(app.locale, "settings.namespaces", "Namespaces"),
        SettingsSection::Models => t_or(app.locale, "settings.models", "Models"),
        SettingsSection::Plugins => t_or(app.locale, "settings.plugins", "Plugins"),
        SettingsSection::General => t_or(app.locale, "settings.general", "General"),
    };
    let title = format!("{} · {section}", t_or(app.locale, "settings.title", "Settings"));
    let title = title.as_str();
    let block = pane_block(title, focused, app);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.settings_section == SettingsSection::Models {
        render_models(frame, inner, app);
        return;
    }
    if app.settings_section == SettingsSection::Plugins {
        render_plugins(frame, inner, app);
        return;
    }
    if app.settings_section == SettingsSection::General {
        render_general(frame, inner, app);
        return;
    }

    let Some(describe) = app.settings.as_ref() else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                t_or(app.locale, "settings.reading", "Reading the settings document…"),
                Style::default().fg(app.theme.text_dim),
            ))),
            inner,
        );
        return;
    };

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(26), Constraint::Min(20)])
        .split(inner);

    let namespaces: Vec<ListItem> = describe
        .namespaces
        .iter()
        .enumerate()
        .map(|(index, ns)| {
            let selected = index == app.settings_ns;
            ListItem::new(Line::from(Span::styled(
                ns.ns.clone(),
                if selected {
                    Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.text_dim)
                },
            )))
        })
        .collect();
    frame.render_widget(List::new(namespaces), columns[0]);

    let mut lines: Vec<Line> = Vec::new();
    if !describe.writable {
        // Every write control is inert when the provider refuses writes; saying so beats
        // letting an edit fail one keystroke later.
        lines.push(Line::from(Span::styled(
            t_or(app.locale, "settings.readOnly", "This settings provider is read-only."),
            Style::default().fg(app.theme.warning),
        )));
    }
    if let Some(view) = app.settings_namespace() {
        if view.applies == "restart" {
            lines.push(Line::from(Span::styled(
                t_or(app.locale, "settings.restart", "Changes apply after a restart."),
                Style::default().fg(app.theme.text_dim),
            )));
        }
    }
    if let Some(error) = app.settings_error.as_ref() {
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(app.theme.danger),
        )));
    }

    for (index, field) in app.settings_fields().iter().enumerate() {
        let selected = index == app.settings_field;
        let name_style = if selected {
            Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.theme.text)
        };
        let mut spans = vec![
            Span::styled(if selected { "▸ " } else { "  " }, name_style),
            Span::styled(field.label.clone(), name_style),
            Span::styled("  ", Style::default()),
            Span::styled(
                field.display_value(),
                Style::default().fg(app.theme.text_dim),
            ),
        ];
        // A user override is the one fact the resolved value alone cannot show.
        if field.overridden {
            spans.push(Span::styled(
                "  ·overridden",
                Style::default().fg(app.theme.accent),
            ));
        }
        if let Editor::Unsupported(kind) = &field.editor {
            spans.push(Span::styled(
                format!("  ·{kind}, read-only here"),
                Style::default().fg(app.theme.warning),
            ));
        }
        lines.push(Line::from(spans));
        if selected && !field.description.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("    {}", field.description),
                Style::default().fg(app.theme.text_dim),
            )));
        }
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), columns[1]);
}

/// The Models page: one row per configurable route, with its readiness.
fn render_models(frame: &mut Frame, area: Rect, app: &App) {
    let rows = app.model_rows();
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                t_or(app.locale, "models.reading", "Reading the provider directory…"),
                Style::default().fg(app.theme.text_dim),
            ))),
            area,
        );
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    if let Some(error) = app.credential_error.as_ref() {
        // The rows still render: credential state is an enrichment, not a precondition.
        lines.push(Line::from(Span::styled(
            format!("{}: {error}", t_or(app.locale, "models.credentialsUnavailable", "Credential state unavailable")),
            Style::default().fg(app.theme.warning),
        )));
    }

    for (index, row) in rows.iter().enumerate() {
        let selected = index == app.models_row;
        let name_style = if selected {
            Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.theme.text)
        };
        let status_color = if row.ready() {
            app.theme.success
        } else if row.configured {
            app.theme.warning
        } else {
            app.theme.text_dim
        };
        lines.push(Line::from(vec![
            Span::styled(if selected { "▸ " } else { "  " }, name_style),
            Span::styled(row.display_name.clone(), name_style),
            Span::styled("  ", Style::default()),
            Span::styled(row.status(), Style::default().fg(status_color)),
        ]));
        if selected {
            let source = row
                .credential
                .as_ref()
                .and_then(|c| c.source.clone())
                .unwrap_or_else(|| "unset".to_string());
            lines.push(Line::from(Span::styled(
                format!(
                    "    key {} ({}) · from {source}{}",
                    row.key_ref,
                    if row.key_ref_named { "named by profile" } else { "derived" },
                    if row.removable { " · removable" } else { "" },
                ),
                Style::default().fg(app.theme.text_dim),
            )));
        }
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// The General page: appearance and the product rows no plugin owns.
fn render_general(frame: &mut Frame, area: Rect, app: &App) {
    let lines = vec![
        Line::from(Span::styled(
            t_or(app.locale, "general.appearance", "Appearance"),
            Style::default().fg(app.theme.text).add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("  theme  ", Style::default().fg(app.theme.text)),
            Span::styled(
                app.theme_preference.as_str(),
                Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  ({})", app.theme_reason),
                Style::default().fg(app.theme.text_dim),
            ),
        ]),
        Line::from(Span::styled(
            "    t to cycle · shared with the web UI through the ui-theme namespace",
            Style::default().fg(app.theme.text_dim),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  font size  ", Style::default().fg(app.theme.text)),
            // The web row sets a px size; a terminal's font belongs to the terminal, and
            // offering a control that cannot work would be worse than saying so.
            Span::styled(
                "owned by your terminal",
                Style::default().fg(app.theme.text_dim),
            ),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// The read-only Loader inventory, with its search box and a summary line.
fn render_plugins(frame: &mut Frame, area: Rect, app: &App) {
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(area);

    let summary = app.inventory.summary();
    let mut header = vec![
        Span::styled("search ", Style::default().fg(app.theme.text_dim)),
        Span::styled(
            if app.inventory.query.is_empty() {
                "(type to filter)".to_string()
            } else {
                app.inventory.query.clone()
            },
            Style::default().fg(if app.inventory.query.is_empty() {
                app.theme.text_dim
            } else {
                app.theme.accent
            }),
        ),
    ];
    header.push(Span::styled(
        format!("   {} entries · {} active", summary.total, summary.active),
        Style::default().fg(app.theme.text_dim),
    ));
    if summary.problems > 0 {
        // Disabled entries are expected; failed or fiberless ones are not.
        header.push(Span::styled(
            format!(" · {} need attention", summary.problems),
            Style::default().fg(app.theme.warning),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(header)), split[0]);

    let rows = app.inventory.rows();
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                if app.inventory.is_empty() {
                    t_or(app.locale, "plugins.reading", "Reading the Loader inventory…")
                } else {
                    t_or(app.locale, "plugins.noMatch", "No plugin matches that search.")
                },
                Style::default().fg(app.theme.text_dim),
            ))),
            split[1],
        );
        return;
    }

    let items: Vec<ListItem> = rows
        .iter()
        .take(split[1].height as usize)
        .enumerate()
        .map(|(index, entry)| {
            let selected = index == app.inventory_row;
            let health = entry.health();
            let name_style = if selected {
                Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD)
            } else if entry.enabled {
                Style::default().fg(app.theme.text)
            } else {
                Style::default().fg(app.theme.text_dim)
            };
            let health_color = match health {
                Health::Active => app.theme.success,
                Health::Failed | Health::Inert => app.theme.danger,
                Health::Disabled => app.theme.text_dim,
                _ => app.theme.warning,
            };
            ListItem::new(Line::from(vec![
                Span::styled(if selected { "▸ " } else { "  " }, name_style),
                Span::styled(entry.short_name().to_string(), name_style),
                Span::styled("  ", Style::default()),
                Span::styled(health.label(), Style::default().fg(health_color)),
                // An unrecognized phase still shows its raw value rather than hiding it.
                Span::styled(
                    match (health, entry.fiber_phase.as_deref()) {
                        (Health::Unrecognized, Some(phase)) => format!(" ({phase})"),
                        _ => String::new(),
                    },
                    Style::default().fg(app.theme.text_dim),
                ),
            ]))
        })
        .collect();
    frame.render_widget(List::new(items), split[1]);
}

/// The workspace browser: each workspace with the sessions accounted to it.
/// Severity colour, shared by the log surface and the trajectory pane's fallback tail.
fn level_colour(level: Level, app: &App) -> ratatui::style::Color {
    match level {
        Level::Error => app.theme.danger,
        Level::Warn => app.theme.warning,
        Level::Info => app.theme.text,
        Level::Debug | Level::Trace => app.theme.text_dim,
    }
}

/// This run's log, filtered and scrollable.
///
/// The pane is a *view of the file*, not a second store: it shows what the file received,
/// with the same records and the same thresholds, so a question answered here is answered
/// the same way by `jq` over the file afterwards.
fn render_logs(frame: &mut Frame, area: Rect, app: &App) {
    let block = pane_block(t_or(app.locale, "logs.title", "Logs"), true, app);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(inner);

    // Header: where the records are going, and what is being hidden. Both questions get
    // asked the moment a pane looks emptier than expected.
    let destination = app
        .log_path_short()
        .unwrap_or_else(|| "not writing a file (DSH_TUI_LOG=off)".to_string());
    let payloads = if logging::payloads() {
        Span::styled(
            "payloads:full",
            Style::default().fg(app.theme.warning).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled("payloads:redacted", Style::default().fg(app.theme.text_dim))
    };
    let visible = app.visible_logs();
    let mut summary = vec![
        Span::styled(
            format!("{} of {} records", visible.len(), app.log.len()),
            Style::default().fg(app.theme.text),
        ),
        Span::styled("  ·  ", Style::default().fg(app.theme.text_dim)),
        Span::styled(
            format!("level:{}", app.log_min.as_str()),
            Style::default().fg(app.theme.accent),
        ),
        Span::styled("  ·  ", Style::default().fg(app.theme.text_dim)),
        payloads,
    ];
    if !app.log_filter.is_empty() {
        summary.push(Span::styled("  ·  ", Style::default().fg(app.theme.text_dim)));
        summary.push(Span::styled(
            format!("filter:{}", app.log_filter),
            Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                destination,
                Style::default().fg(app.theme.text_dim),
            )),
            Line::from(summary),
        ]),
        rows[0],
    );

    let body = rows[1];
    if visible.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                t_or(app.locale, "logs.empty", "No records match."),
                Style::default().fg(app.theme.text_dim),
            ))),
            body,
        );
        return;
    }

    let (start, end) = app.log_scroll.window(visible.len());
    let width = body.width as usize;
    let origin = logging::started();
    let lines: Vec<Line> = visible[start..end]
        .iter()
        .map(|record| {
            // One record is one row: a wrapped log makes scrolling unpredictable and
            // hides the alignment that lets the eye scan levels and timings down a column.
            let text = truncate(&record.render(origin), width);
            let style = Style::default().fg(level_colour(record.level, app));
            let style = if record.src == Source::Host {
                style.add_modifier(Modifier::ITALIC)
            } else {
                style
            };
            Line::from(Span::styled(text, style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), body);
}

/// Clip to `width`, marking that something was cut. The full record is in the file.
fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn render_workspace(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Pane::Conversation;
    let block = pane_block(t_or(app.locale, "workspaces.title", "Workspaces"), focused, app);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.workspaces.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                t_or(app.locale, "workspaces.empty", "No workspaces yet."),
                Style::default().fg(app.theme.text_dim),
            ))),
            inner,
        );
        return;
    }

    let mut items: Vec<ListItem> = Vec::new();
    for (index, row) in app.workspaces.rows().iter().enumerate() {
        let selected = index == app.workspace_row;
        // The one currently rooting new sessions, which is not necessarily the one the
        // cursor is on.
        let active = app.workspace.as_deref() == Some(row.path.as_str());
        let title_style = if selected {
            Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.theme.text).add_modifier(Modifier::BOLD)
        };
        let titled = row.title != row.path;
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(vec![
            Span::styled(if selected { "▸ " } else { "  " }, title_style),
            Span::styled(row.title.clone(), title_style),
            Span::styled(
                if titled { format!("  {}", row.path) } else { String::new() },
                Style::default().fg(app.theme.text_dim),
            ),
            Span::styled(
                if active { "  ● in use" } else { "" },
                Style::default().fg(app.theme.success),
            ),
        ]));
        for session in &row.session_ids {
            lines.push(Line::from(Span::styled(
                format!("     {session}"),
                Style::default().fg(app.theme.text_dim),
            )));
        }
        lines.push(Line::from(""));
        // One item per workspace, so the cursor and the scroll window agree on what a
        // row is even though a row is several lines tall.
        items.push(ListItem::new(lines));
    }
    let mut state = ListState::default();
    state.select(Some(app.workspace_row));
    frame.render_stateful_widget(List::new(items), inner, &mut state);
}

fn render_details(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.focus == Pane::Details;
    let block = pane_block(t_or(app.locale, "trajectory.title", "Trajectory"), focused, app);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let turns = trajectory::turns(&app.ledger);
    if turns.is_empty() {
        let lines: Vec<Line> = if app.log.is_empty() {
            vec![Line::from(Span::styled(
                t_or(app.locale, "trajectory.empty", "No turns yet."),
                Style::default().fg(app.theme.text_dim),
            ))]
        } else {
            let start = logging::started();
            app.log
                .iter()
                .rev()
                .take(inner.height as usize)
                .rev()
                .map(|record| {
                    Line::from(Span::styled(
                        record.render(start),
                        Style::default().fg(level_colour(record.level, app)),
                    ))
                })
                .collect()
        };
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
        return;
    }

    // Bars compare turns with each other; an absolute scale means nothing in a terminal.
    let longest = turns.iter().filter_map(|turn| turn.millis).max().unwrap_or(0);
    let bar_width = (inner.width as usize).saturating_sub(28).clamp(4, 16);

    let mut lines: Vec<Line> = Vec::new();
    for turn in &turns {
        let label = if turn.index == 0 {
            "before resume".to_string()
        } else {
            format!("turn {}", turn.index)
        };
        let duration = trajectory::format_millis(turn.millis);
        let duration_style = if turn.is_open() {
            Style::default().fg(app.theme.warning)
        } else {
            Style::default().fg(app.theme.text_dim)
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{label:<14}"),
                Style::default().fg(app.theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{duration:>8}  "), duration_style),
            Span::styled(
                trajectory::bar(turn.millis.unwrap_or(0), longest, bar_width),
                Style::default().fg(app.theme.accent),
            ),
        ]));

        for tool in &turn.tools {
            let style = if tool.failed {
                Style::default().fg(app.theme.danger)
            } else {
                Style::default().fg(app.theme.text_dim)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<12}", tool.name), style),
                Span::styled(
                    format!("{:>8}", trajectory::format_millis(tool.millis)),
                    style,
                ),
                Span::styled(
                    if tool.failed { "  failed" } else { "" },
                    Style::default().fg(app.theme.danger),
                ),
            ]));
        }
    }

    for run in app.workflow_runs() {
        let color = if run.failed() {
            app.theme.danger
        } else if run.is_running() {
            app.theme.warning
        } else {
            app.theme.success
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("workflow {}  ", run.name),
                Style::default().fg(app.theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(run.status().to_string(), Style::default().fg(color)),
        ]));
        for (phase, members) in run.phases() {
            if let Some(phase) = phase {
                lines.push(Line::from(Span::styled(
                    format!("  {phase}"),
                    Style::default().fg(app.theme.text_dim),
                )));
            }
            for member in members {
                let member_color = if member.failed() {
                    app.theme.danger
                } else if member.is_running() {
                    app.theme.warning
                } else {
                    app.theme.text_dim
                };
                lines.push(Line::from(Span::styled(
                    format!("    {:<20}{}", member.label, member.status()),
                    Style::default().fg(member_color),
                )));
            }
        }
    }

    let produced = app.produced_files();
    if !produced.is_empty() {
        lines.push(Line::from(Span::styled(
            t_or(app.locale, "deliverables.produced", "Produced"),
            Style::default().fg(app.theme.text).add_modifier(Modifier::BOLD),
        )));
        for path in produced.iter().take(6) {
            lines.push(Line::from(Span::styled(
                format!("  {path}"),
                Style::default().fg(app.theme.accent),
            )));
        }
        if produced.len() > 6 {
            lines.push(Line::from(Span::styled(
                format!("  + {} more", produced.len() - 6),
                Style::default().fg(app.theme.text_dim),
            )));
        }
    }

    // The tail is what a running session is about.
    let visible = inner.height as usize;
    if lines.len() > visible {
        lines = lines.split_off(lines.len() - visible);
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    let (label, color) = match &app.connection {
        Connection::Connecting => ("connecting", app.theme.warning),
        Connection::Ready(_) => ("connected", app.theme.success),
        Connection::Failed(_) => ("disconnected", app.theme.danger),
    };
    let hints = match app.view {
        View::Conversation if app.focus == Pane::Sidebar => {
            "  ↑↓ session · enter open · n new · tab focus · ^w workspaces · ^c quit"
        }
        View::Conversation => {
            "  tab focus · ^b sidebar · ^d details · ^p model · ^s settings · ^w workspaces · ^c quit"
        }
        View::Settings => "  ↑↓ row · ←→ namespace · tab page · esc back · ^c quit",
        View::Workspace => "  ↑↓ row · enter use · n new workspace · esc back · ^c quit",
        View::Logs => {
            "  type to filter · tab level · ↑↓ pgup/pgdn scroll · ^y copy path · esc back"
        }
    };
    let mut spans = vec![Span::styled(format!(" {label} "), Style::default().fg(color))];
    match app.workspace_label() {
        Some(workspace) => spans.push(Span::styled(
            format!("{workspace}  "),
            Style::default().fg(app.theme.accent),
        )),
        // Not an error — it is the state every run starts in — but it is the reason `n`
        // will open a picker instead of starting a session, so it is worth saying.
        None => spans.push(Span::styled(
            "no workspace  ",
            Style::default().fg(app.theme.warning),
        )),
    }
    // Say when the view is held above the newest output, so silence is not mistaken for
    // the agent having stopped.
    if !app.scroll.is_following() {
        spans.push(Span::styled(
            "scrolled  ",
            Style::default().fg(app.theme.warning),
        ));
    }
    spans.push(Span::styled(hints, Style::default().fg(app.theme.text_dim)));
    let line = Line::from(spans);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(app.theme.bg_base)),
        area,
    );
}

/// The composer takeover: one question at a time, with its options.
fn render_questions(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    pending: &crate::app::PendingQuestions,
) {
    let Some(question) = pending.current() else { return };
    let plan_review = question.plan_review_approve();

    let width = area.width.saturating_sub(6).clamp(1, 90);
    let height = area.height.saturating_sub(4).clamp(1, 20);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);

    let title = if plan_review.is_some() {
        " Plan review "
    } else {
        " Question "
    };
    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(app.theme.accent))
        .style(Style::default().bg(app.theme.bg_layer));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::new();
    if pending.request.questions.len() > 1 {
        lines.push(Line::from(Span::styled(
            format!(
                "{} of {}",
                pending.question_index + 1,
                pending.request.questions.len()
            ),
            Style::default().fg(app.theme.text_dim),
        )));
    }
    if let Some(header) = question.header.as_ref() {
        lines.push(Line::from(Span::styled(
            header.clone(),
            Style::default().fg(app.theme.text_dim),
        )));
    }
    lines.push(Line::from(Span::styled(
        question.question.clone(),
        Style::default().fg(app.theme.text).add_modifier(Modifier::BOLD),
    )));
    if let Some(detail) = question.detail.as_ref() {
        for line in detail.lines() {
            lines.push(Line::from(Span::styled(
                format!("  {line}"),
                Style::default().fg(app.theme.text_dim),
            )));
        }
    }
    lines.push(Line::from(""));

    let selected = pending.draft.selected(&question.id);
    for (index, option) in question.options.iter().enumerate() {
        let focused = index == pending.option_index;
        let chosen = selected.iter().any(|label| label == &option.label);
        // The approving option is named by the request, never inferred from its position.
        let approves = question.approves(&option.label);
        let color = if approves {
            app.theme.success
        } else if plan_review.is_some() {
            app.theme.danger
        } else {
            app.theme.text
        };
        let style = if focused {
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(color)
        };
        let mut spans = vec![
            Span::styled(if focused { "▸ " } else { "  " }, style),
            Span::styled(if chosen { "[x] " } else { "[ ] " }, style),
            Span::styled(option.label.clone(), style),
        ];
        if let Some(description) = option.description.as_ref() {
            spans.push(Span::styled(
                format!("  {description}"),
                Style::default().fg(app.theme.text_dim),
            ));
        }
        lines.push(Line::from(spans));
    }

    if question.multi_select {
        lines.push(Line::from(Span::styled(
            "  choose as many as apply",
            Style::default().fg(app.theme.text_dim),
        )));
    }

    lines.push(Line::from(""));
    let hint = if pending.is_complete() {
        "space choose · ↑↓ option · ←→ question · enter submit · d delegate"
    } else {
        "space choose · ↑↓ option · ←→ question · d delegate"
    };
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(app.theme.text_dim),
    )));

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The Miller-column workspace directory chooser, as a centred dialog.
fn render_directory_picker(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    picker: &crate::directory::Browser,
) {
    let width = area.width.saturating_sub(8).clamp(1, 82);
    let height = area.height.saturating_sub(4).clamp(1, 18);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(Span::styled(
            format!(" {} ", picker.breadcrumb()),
            Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(app.theme.accent))
        .style(Style::default().bg(app.theme.bg_layer));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // The list scrolls, so the note and the footer get their own rows rather than being
    // pushed onto the end of it — otherwise they scroll away with the entries.
    let note = picker.truncation_note();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(u16::from(note.is_some())),
            Constraint::Length(u16::from(app.picker_error.is_some())),
            Constraint::Length(1),
        ])
        .split(inner);

    // Row 0 stands for the directory being browsed, so "use this one" is a thing the
    // cursor can point at rather than a rule the reader has to remember.
    let use_selected = picker.on_use_row();
    let use_style = if use_selected {
        Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.theme.success)
    };
    let mut items: Vec<ListItem> = vec![ListItem::new(Line::from(vec![
        Span::styled(if use_selected { "▸ " } else { "  " }, use_style),
        Span::styled(
            t_or(app.locale, "picker.useThis", "✓ use this directory"),
            use_style,
        ),
        Span::styled(
            format!("  {}", picker.breadcrumb()),
            Style::default().fg(app.theme.text_dim),
        ),
    ]))];
    for (index, entry) in picker.rows().iter().enumerate() {
        // Entry rows sit after the synthetic one, so the cursor is one ahead of them.
        let selected = index + 1 == picker.selected;
        let style = if selected {
            Style::default().fg(app.theme.accent).add_modifier(Modifier::BOLD)
        } else if entry.hidden {
            Style::default().fg(app.theme.text_dim)
        } else {
            Style::default().fg(app.theme.text)
        };
        items.push(ListItem::new(Line::from(vec![
            Span::styled(if selected { "▸ " } else { "  " }, style),
            Span::styled(entry.name.clone(), style),
        ])));
    }

    // A stateful list so the window follows the cursor. Drawn as a plain `Paragraph` the
    // rows past the popup's height were simply clipped, and the cursor walked off the
    // bottom with nothing to show for it.
    let mut state = ListState::default();
    state.select(Some(picker.selected));
    frame.render_stateful_widget(List::new(items), rows[0], &mut state);

    if let Some(note) = note {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {note}"),
                Style::default().fg(app.theme.warning),
            ))),
            rows[1],
        );
    }
    // A refused or impossible navigation says why, where the reader is looking.
    if let Some(error) = app.picker_error.as_deref() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {error}"),
                Style::default().fg(app.theme.danger),
            ))),
            rows[2],
        );
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "  enter select · → open · ← up · pgup/pgdn · . hidden · esc cancel",
            Style::default().fg(app.theme.text_dim),
        ))),
        rows[3],
    );
}

fn render_ask_modal(frame: &mut Frame, area: Rect, app: &App, ask: &crate::app::PendingAsk) {
    // Both dimensions stay inside `area`: raising a floor above the available size would
    // draw the modal off-screen on a small terminal.
    let width = area.width.saturating_sub(8).clamp(1, 76);
    let height = area.height.saturating_sub(2).clamp(1, 11);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(Span::styled(
            format!(" {} ", t_or(app.locale, "approval.title", "Permission required")),
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(app.theme.accent))
        .style(Style::default().bg(app.theme.bg_layer));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines = vec![
        Line::from(Span::styled(
            ask.summary(),
            Style::default().fg(app.theme.text),
        )),
        Line::from(""),
    ];

    if ask.is_renderable() {
        lines.push(Line::from(vec![
            Span::styled("y", Style::default().fg(app.theme.success)),
            Span::styled(" allow    ", Style::default().fg(app.theme.text_dim)),
            Span::styled("n", Style::default().fg(app.theme.danger)),
            Span::styled(" deny    ", Style::default().fg(app.theme.text_dim)),
            Span::styled("d", Style::default().fg(app.theme.accent)),
            Span::styled(" delegate to host", Style::default().fg(app.theme.text_dim)),
        ]));
    } else {
        // Deciding on the user's behalf for a shape this build cannot show would be worse
        // than handing the request back to the host's own listener.
        lines.push(Line::from(Span::styled(
            t_or(app.locale, "approval.unrenderable", "This build cannot render this request."),
            Style::default().fg(app.theme.warning),
        )));
        lines.push(Line::from(vec![
            Span::styled("d", Style::default().fg(app.theme.accent)),
            Span::styled(
                " delegate to host (only safe reply)",
                Style::default().fg(app.theme.text_dim),
            ),
        ]));
    }

    if app.asks.len() > 1 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("{} more waiting", app.asks.len() - 1),
            Style::default().fg(app.theme.text_dim),
        )));
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}
