//! Key routing.
//!
//! Lives in the library rather than the binary so it can be tested: a binding that never
//! fires is invisible to a test that calls the `App` method directly, and that is exactly
//! how `^w`'s arrow keys shipped broken.
//!
//! ## Order matters
//!
//! Each block below `return`s, so the **first** matching block owns the keystroke. Modals
//! are therefore checked before the full-column views: the directory picker can be opened
//! from the workspace list, and if the view were checked first it would keep consuming
//! keys while the picker sat on screen.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{App, AskReply, Pane, ScrollDelta, SearchEdit, View};
use crate::transport::Transport;

pub fn copy_to_clipboard(text: String) {
    use std::io::Write as _;
    use base64::Engine as _;

    if text.is_empty() {
        return;
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let mut out = std::io::stdout();
    let _ = out.write_all(format!("\x1b]52;c;{encoded}\x07").as_bytes());
    let _ = out.flush();
}

pub fn on_key(app: &mut App, transport: &mut Transport, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // A blocked waterfall takes every keystroke: the agent is waiting on this answer.
    if !app.asks.is_empty() {
        let renderable = app.asks.front().map(|a| a.is_renderable()).unwrap_or(false);
        match key.code {
            KeyCode::Char('y') if renderable => app.answer_ask(AskReply::Allow, transport),
            KeyCode::Char('n') if renderable => app.answer_ask(AskReply::Deny, transport),
            KeyCode::Char('d') => app.answer_ask(AskReply::Delegate, transport),
            KeyCode::Char('c') if ctrl => app.should_quit = true,
            _ => {}
        }
        return;
    }

    // A pending question request owns the keyboard: the agent is blocked on it.
    if app.questions.is_some() {
        match key.code {
            KeyCode::Char('c') if ctrl => app.should_quit = true,
            KeyCode::Up => app.prev_option(),
            KeyCode::Down => app.next_option(),
            KeyCode::Left => app.prev_question(),
            KeyCode::Right => app.next_question(),
            KeyCode::Char(' ') => app.choose_option(),
            KeyCode::Enter => app.submit_questions(transport),
            // Delegating hands the request to the host's own answerer rather than
            // inventing a reply the human never gave.
            KeyCode::Char('d') => app.delegate_questions(transport),
            _ => {}
        }
        return;
    }
    // The directory chooser owns the keyboard while it is open.
    if app.picker.is_some() {
        match key.code {
            KeyCode::Char('c') if ctrl => app.should_quit = true,
            KeyCode::Esc => app.cancel_picker(),
            KeyCode::Up => {
                if let Some(picker) = app.picker.as_mut() {
                    picker.select_prev();
                }
            }
            KeyCode::Down => {
                if let Some(picker) = app.picker.as_mut() {
                    picker.select_next();
                }
            }
            KeyCode::Char('.') => {
                if let Some(picker) = app.picker.as_mut() {
                    picker.toggle_hidden();
                }
            }
            // A long directory has to be reachable: without these the cursor could only
            // crawl, and the rows past the popup's height were unreachable entirely.
            KeyCode::PageDown => {
                let page = app.picker_page;
                if let Some(picker) = app.picker.as_mut() {
                    picker.page_down(page);
                }
            }
            KeyCode::PageUp => {
                let page = app.picker_page;
                if let Some(picker) = app.picker.as_mut() {
                    picker.page_up(page);
                }
            }
            KeyCode::Home => {
                if let Some(picker) = app.picker.as_mut() {
                    picker.select_first();
                }
            }
            KeyCode::End => {
                if let Some(picker) = app.picker.as_mut() {
                    picker.select_last();
                }
            }
            // `enter` commits the directory being browsed, and `→` descends into the
            // highlighted row. Before this the dialog could only ever navigate: there was
            // no key that chose anything, which is why nothing ever set the workspace.
            KeyCode::Enter => app.choose_directory(transport),
            KeyCode::Right => app.enter_directory(transport),
            KeyCode::Left => app.leave_directory(transport),
            _ => {}
        }
        return;
    }
    // The model picker owns the keyboard while it is open.
    if app.model_picker.is_some() {
        match key.code {
            KeyCode::Char('c') if ctrl => app.should_quit = true,
            KeyCode::Esc => app.close_model_picker(),
            KeyCode::Up => app.select_prev_model(),
            KeyCode::Down => app.select_next_model(),
            KeyCode::Enter | KeyCode::Tab => app.pick_model(transport),
            KeyCode::Backspace => app.search_models(SearchEdit::Backspace),
            KeyCode::Char(c) if !ctrl => app.search_models(SearchEdit::Push(c)),
            _ => {}
        }
        return;
    }
    // The search bar owns typing while it is open.
    if app.search.is_some() {
        match key.code {
            KeyCode::Char('c') if ctrl => app.should_quit = true,
            KeyCode::Esc => app.close_search(),
            // Navigation is on modified keys: the query field owns plain characters, so a
            // bare `n` must type an `n` rather than jump.
            KeyCode::Char('n') if ctrl => app.step_match(true),
            KeyCode::Char('p') if ctrl => app.step_match(false),
            KeyCode::Enter => app.step_match(true),
            KeyCode::Backspace => app.edit_search(SearchEdit::Backspace),
            KeyCode::Char(c) if !ctrl => app.edit_search(SearchEdit::Push(c)),
            _ => {}
        }
        return;
    }
    if app.view == View::Logs {
        match key.code {
            KeyCode::Char('c') if ctrl => app.should_quit = true,
            // Esc clears an active filter before it leaves the surface, so a narrowed
            // pane is never mistaken for an empty log.
            KeyCode::Esc if !app.log_filter.is_empty() => app.edit_log_filter(SearchEdit::Clear),
            KeyCode::Esc => app.show_conversation(),
            KeyCode::Tab => app.cycle_log_level(),
            KeyCode::Char('y') if ctrl => copy_to_clipboard(app.log_path().unwrap_or_default()),
            KeyCode::Up => app.scroll_logs(ScrollDelta::LineUp),
            KeyCode::Down => app.scroll_logs(ScrollDelta::LineDown),
            KeyCode::PageUp => app.scroll_logs(ScrollDelta::PageUp),
            KeyCode::PageDown => app.scroll_logs(ScrollDelta::PageDown),
            KeyCode::Home => app.scroll_logs(ScrollDelta::Top),
            KeyCode::End => app.scroll_logs(ScrollDelta::Bottom),
            KeyCode::Backspace => app.edit_log_filter(SearchEdit::Backspace),
            // The keyboard is a filter box here, so plain characters narrow the pane
            // rather than navigate it.
            KeyCode::Char(c) if !ctrl => app.edit_log_filter(SearchEdit::Push(c)),
            _ => {}
        }
        return;
    }
    // Settings and the workspace browser own the centre column while open.
    if app.view != View::Conversation {
        match key.code {
            KeyCode::Char('c') if ctrl => app.should_quit = true,
            // Esc clears an active search before it leaves the page.
            KeyCode::Esc if app.is_plugins_page() && !app.inventory.query.is_empty() => {
                app.search_inventory(SearchEdit::Clear)
            }
            KeyCode::Esc => app.show_conversation(),
            KeyCode::Tab if app.view == View::Settings => {
                app.next_settings_section(transport)
            }
            KeyCode::Char('t') if app.is_general_page() => app.cycle_theme(transport),
            // The workspace list: move, take one, or register a new one. Until now this
            // surface had no keys at all, so it could show a workspace but never use it.
            KeyCode::Up if app.view == View::Workspace => app.select_prev_workspace(),
            KeyCode::Down if app.view == View::Workspace => app.select_next_workspace(),
            KeyCode::Enter if app.view == View::Workspace => {
                app.use_selected_workspace(transport)
            }
            // A workspace row is several lines tall, so a fixed page size would be a
            // guess; Home/End are exact and cover a long roster.
            KeyCode::Home if app.view == View::Workspace => app.workspace_row = 0,
            KeyCode::End if app.view == View::Workspace => app.select_last_workspace(),
            KeyCode::Char('n') if app.view == View::Workspace && !ctrl => {
                app.create_workspace(transport)
            }
            KeyCode::Up if app.is_models_page() => app.select_prev_model_row(),
            KeyCode::Down if app.is_models_page() => app.select_next_model_row(),
            KeyCode::Up if app.is_plugins_page() => app.select_prev_inventory_row(),
            KeyCode::Down if app.is_plugins_page() => app.select_next_inventory_row(),
            // On the inventory page the keyboard is a search box, so plain characters
            // filter rather than navigate.
            KeyCode::Backspace if app.is_plugins_page() => {
                app.search_inventory(SearchEdit::Backspace)
            }
            KeyCode::Char(c) if app.is_plugins_page() && !ctrl => {
                app.search_inventory(SearchEdit::Push(c))
            }
            KeyCode::Up if app.view == View::Settings => app.select_prev_field(),
            KeyCode::Down if app.view == View::Settings => app.select_next_field(),
            KeyCode::Left if app.view == View::Settings => {
                app.select_prev_namespace()
            }
            KeyCode::Right if app.view == View::Settings => {
                app.select_next_namespace()
            }
            _ => {}
        }
        return;
    }






    match key.code {
        KeyCode::Char('c') if ctrl => app.should_quit = true,
        KeyCode::Char('p') if ctrl => app.open_model_picker(transport),
        KeyCode::Char('f') if ctrl => app.open_search(),
        KeyCode::PageUp => app.scroll_conversation(ScrollDelta::PageUp, transport),
        KeyCode::PageDown => app.scroll_conversation(ScrollDelta::PageDown, transport),
        KeyCode::Home if app.focus == Pane::Conversation && app.composer.is_empty() => {
            app.scroll_conversation(ScrollDelta::Top, transport)
        }
        KeyCode::End if app.focus == Pane::Conversation && app.composer.is_empty() => {
            app.scroll_conversation(ScrollDelta::Bottom, transport)
        }
        KeyCode::Up if app.focus == Pane::Conversation => {
            app.scroll_conversation(ScrollDelta::LineUp, transport)
        }
        KeyCode::Down if app.focus == Pane::Conversation => {
            app.scroll_conversation(ScrollDelta::LineDown, transport)
        }
        KeyCode::Char('o') if ctrl => app.browse_directory(None, transport),
        KeyCode::Char('s') if ctrl => app.open_settings(transport),
        KeyCode::Char('w') if ctrl => app.open_workspace_list(transport),
        KeyCode::Char('l') if ctrl => app.open_logs(),
        KeyCode::Char('m') if ctrl => app.open_models(transport),
        KeyCode::Char('n') if ctrl => app.new_session(transport),
        // The sidebar's own `n`, matching the empty-list hint.
        KeyCode::Char('n') if app.focus == Pane::Sidebar => app.new_session(transport),
        KeyCode::Char('b') if ctrl => app.toggle_sidebar(),
        KeyCode::Char('d') if ctrl => app.toggle_details(),
        KeyCode::Up if app.focus == Pane::Sidebar => app.select_prev_session(transport),
        KeyCode::Down if app.focus == Pane::Sidebar => app.select_next_session(transport),
        // Enter opens the highlighted session and moves focus to the composer, so the
        // next keystroke continues the conversation rather than moving the cursor again.
        KeyCode::Enter if app.focus == Pane::Sidebar => app.open_selected_session(transport),
        KeyCode::Home if app.focus == Pane::Sidebar => app.select_first_session(transport),
        KeyCode::End if app.focus == Pane::Sidebar => app.select_last_session(transport),
        // The candidate menu owns navigation keys while it is open.
        KeyCode::Up if app.focus == Pane::Conversation && !app.candidates.is_empty() => {
            app.select_prev_candidate()
        }
        KeyCode::Down if app.focus == Pane::Conversation && !app.candidates.is_empty() => {
            app.select_next_candidate()
        }
        KeyCode::Tab if app.focus == Pane::Conversation && !app.candidates.is_empty() => {
            app.pick_candidate(transport)
        }
        KeyCode::Esc if !app.candidates.is_empty() => app.candidates.clear(),
        // Only once no menu is open does Tab mean "move focus".
        KeyCode::Tab => app.focus_next(),
        KeyCode::Enter if app.focus == Pane::Conversation && !app.candidates.is_empty() => {
            app.pick_candidate(transport)
        }
        KeyCode::Left if app.focus == Pane::Conversation => app.composer.move_left(),
        KeyCode::Right if app.focus == Pane::Conversation => app.composer.move_right(),
        KeyCode::Home if app.focus == Pane::Conversation => app.composer.move_home(),
        KeyCode::End if app.focus == Pane::Conversation => app.composer.move_end(),
        KeyCode::Enter if app.focus == Pane::Conversation => {
            app.submit_prompt(transport);
            // A submitted message leaves no trigger behind, so the menu must not linger.
            app.refresh_candidates(transport);
        }
        KeyCode::Backspace if app.focus == Pane::Conversation => {
            app.composer.backspace();
            app.refresh_candidates(transport);
        }
        KeyCode::Char(c) if app.focus == Pane::Conversation && !ctrl => {
            app.composer.insert(c);
            app.refresh_candidates(transport);
        }
        _ => {}
    }
}
