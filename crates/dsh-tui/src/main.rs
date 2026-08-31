//! `dsh-tui` — a terminal front end for DSH, with the feature surface of `dsh web`.

use anyhow::Result;
use dsh_tui::app::{App, Connection};
use dsh_tui::keys::on_key;
use dsh_tui::logging::{self, Record};
use dsh_tui::{theme, transport, ui};
use crossterm::event::{Event, EventStream};
use theme::{Mode, Theme};
use tokio_stream::StreamExt;
use transport::Transport;

/// Launcher options. The runtime command is overridable so the TUI can be driven against
/// a stub bridge during development.
struct Options {
    program: String,
    args: Vec<String>,
    theme: Mode,
    /// Render one frame as plain text after this many seconds, then exit.
    ///
    /// Lets the whole stack be exercised without a terminal — useful in CI and for
    /// checking a change against a real harness.
    screenshot: Option<u64>,
    size: (u16, u16),
    /// Which surface to open before the screenshot is taken.
    view: Option<String>,
}

/// What `--help` prints. Kept next to the parser so the two cannot drift.
const USAGE: &str = "\
dsh-tui — a terminal front end for DSH.

Usage:
  dsh-tui [options]
  dsh-tui [options] --runtime <command> [args…]

Options:
  --light | --dark          colour scheme (default: dark)
  --screenshot <seconds>    render one frame as plain text after N seconds, then exit
  --view <surface>          open a surface first: settings, models, plugins, general,
                            workspace, logs, or `rows` to dump the ledger's rows
  --size <cols>x<rows>      frame size for --screenshot (default: 120x32)
  --runtime <cmd> [args…]   replace the spawned bridge; consumes every remaining
                            argument, so it must come last
  -h, --help                show this help
  -V, --version             show the version

Environment:
  DSH_TUI_HOST_COMMAND      program the bridge spawns as the harness host (default: dsh)
  DSH_TUI_HOST_ARGS         its arguments (default: web --no-open --port 0)
  DSH_TUI_HOST_CWD          directory to spawn it in
  DSH_TUI_LOG               error|warn|info|debug|trace|off (default: info)
  DSH_TUI_LOG_PAYLOADS      1 to record full frame bodies rather than their shapes

The default runtime is the bundled bridge. A source checkout runs it as:
  dsh-tui --runtime node bridge/lib/runner.js
";

impl Options {
    fn parse() -> Self {
        // The bridge is the runtime: it boots a harness host and speaks the TUI protocol.
        let mut program = "dsh-tui-bridge".to_string();
        let mut args: Vec<String> = Vec::new();
        let mut theme = Mode::Dark;
        let mut screenshot = None;
        let mut size = (120u16, 32u16);
        let mut view = None;

        let mut argv = std::env::args().skip(1);
        while let Some(arg) = argv.next() {
            match arg.as_str() {
                "--light" => theme = Mode::Light,
                "--dark" => theme = Mode::Dark,
                "--screenshot" => {
                    screenshot = argv.next().and_then(|value| value.parse().ok()).or(Some(3));
                }
                "--view" => view = argv.next(),
                "--size" => {
                    if let Some((w, h)) = argv.next().and_then(|value| {
                        let (w, h) = value.split_once('x')?;
                        Some((w.parse().ok()?, h.parse().ok()?))
                    }) {
                        size = (w, h);
                    }
                }
                "--runtime" => {
                    // Everything after --runtime is the command line to spawn, so it must
                    // be the last flag; anything after it belongs to the child.
                    let rest: Vec<String> = argv.by_ref().collect();
                    if let Some((head, tail)) = rest.split_first() {
                        program = head.clone();
                        args = tail.to_vec();
                    }
                    break;
                }
                // Both exits happen before the terminal is claimed, so they print to a
                // normal stdout rather than into an alternate screen.
                "--help" | "-h" => {
                    print!("{USAGE}");
                    std::process::exit(0);
                }
                "--version" | "-V" => {
                    println!("dsh-tui {}", env!("CARGO_PKG_VERSION"));
                    std::process::exit(0);
                }
                // Silently ignoring an unknown flag hid typos behind a UI that then just
                // looked wrong — `--screenshots 5` rendered a full TUI into a pipe.
                other => {
                    eprintln!("dsh-tui: unrecognised argument `{other}`\n");
                    eprint!("{USAGE}");
                    std::process::exit(2);
                }
            }
        }

        Self { program, args, theme, screenshot, size, view }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let options = Options::parse();

    let log_file = logging::init();
    logging::emit(
        Record::info("app.start")
            .msg(format!(
                "dsh-tui {} starting",
                env!("CARGO_PKG_VERSION")
            ))
            .field("version", env!("CARGO_PKG_VERSION"))
            .field("runtime", options.program.clone())
            .field("args", options.args.clone())
            .field("level", logging::level().as_str())
            .field("payloads", logging::payloads()),
    );

    if let Some(seconds) = options.screenshot {
        return screenshot(options, seconds).await;
    }

    // Restore the terminal even on panic: a renderer fault must never leave the user in
    // raw mode with no echo. The panic is recorded first — the terminal is about to be
    // handed back and whatever scrolls past is the only other copy.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        logging::emit(
            Record::error("app.panic")
                .msg(info.to_string())
                .field(
                    "location",
                    info.location().map(|at| at.to_string()).unwrap_or_default(),
                ),
        );
        ratatui::restore();
        hook(info);
    }));

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, options).await;
    ratatui::restore();

    logging::emit(match &result {
        Ok(()) => Record::info("app.exit").msg("clean exit"),
        Err(error) => Record::error("app.exit").msg(error.to_string()),
    });
    // Say where the evidence is. A TUI that fails after restoring the terminal leaves
    // nothing on screen, and the file is the only place the run survives.
    if let (Some(path), Err(_)) = (&log_file, &result) {
        eprintln!("dsh-tui: log written to {}", path.display());
    }
    result
}

/// How long to wait before saying that the handshake has not landed.
const HANDSHAKE_NOTICE: std::time::Duration = std::time::Duration::from_secs(10);

async fn run(terminal: &mut ratatui::DefaultTerminal, options: Options) -> Result<()> {
    let mut app = App::new(Theme::new(options.theme));
    let (mut transport, mut incoming) = Transport::spawn(&options.program, &options.args)?;
    let mut keys = EventStream::new();

    // A handshake that never lands leaves the conversation on "Starting the harness
    // runtime…" indefinitely. The wait is deliberate — a slow host must not be killed —
    // so the fix is to make the waiting visible rather than to time it out.
    let mut stalled = Box::pin(tokio::time::sleep(HANDSHAKE_NOTICE));

    let size = terminal.size()?;
    app.measure(size.height);
    terminal.draw(|frame| ui::render(frame, &app))?;
    draw_images(&app, size.width, size.height)?;

    loop {
        tokio::select! {
            message = incoming.recv() => match message {
                Some(message) => app.on_incoming(message, &mut transport),
                None => break,
            },
            event = keys.next() => match event {
                Some(Ok(Event::Key(key))) if key.is_press() => {
                    on_key(&mut app, &mut transport, key);
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(error.into()),
                None => break,
            },
            _ = &mut stalled => {
                if matches!(app.connection, Connection::Connecting) {
                    app.record(
                        Record::warn("handshake.stalled")
                            .msg("no ready frame yet; the runtime has not finished starting")
                            .field("seconds", HANDSHAKE_NOTICE.as_secs()),
                    );
                }
                // Rearm, so a long stall is reported as it continues rather than once.
                stalled = Box::pin(tokio::time::sleep(HANDSHAKE_NOTICE));
            }
        }

        if app.should_quit {
            break;
        }
        // Measure before drawing: the viewport's clamp and the search matches both depend
        // on the current line count and pane height.
        let size = terminal.size()?;
        app.measure(size.height);
        terminal.draw(|frame| ui::render(frame, &app))?;
        draw_images(&app, size.width, size.height)?;
    }

    transport.shutdown().await;
    Ok(())
}

/// Drive the app headlessly and print one frame as text.
///
/// No raw mode and no alternate screen: this is for checking the real stack from a script.
async fn screenshot(options: Options, seconds: u64) -> Result<()> {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new(Theme::new(options.theme));
    let (mut transport, mut incoming) = Transport::spawn(&options.program, &options.args)?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);

    // Open the requested surface once the handshake has landed, so its own reads are in
    // flight while the rest of the window is still filling.
    let mut opened = options.view.is_none();

    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, incoming.recv()).await {
            Ok(Some(message)) => app.on_incoming(message, &mut transport),
            Ok(None) => break,
            Err(_) => break,
        }
        if !opened && !app.sessions.is_empty() {
            opened = true;
            match options.view.as_deref() {
                Some("settings") => app.open_settings(&mut transport),
                Some("models") => app.open_models(&mut transport),
                Some("plugins") => app.open_plugins(&mut transport),
                Some("general") => app.open_general(&mut transport),
                Some("workspace") => app.open_workspace(&mut transport),
                Some("logs") => app.open_logs(),
                Some("presets") => app.load_presets(&mut transport),
                Some("rows") => {}
                Some(other) => eprintln!("unknown view: {other}"),
                None => {}
            }
        }
    }

    // The details pane carries the runtime log, which is where a stream failure lands.
    app.details_open = true;
    app.measure(options.size.1);
    if options.view.as_deref() == Some("rows") {
        // Row-level dump: what the ledger produced, before any layout or wrapping.
        for row in app.ledger.rows() {
            println!("{:?}\t{:?}\t{}", row.kind, row.glyph, row.text.replace('\n', "\\n"));
        }
        transport.shutdown().await;
        return Ok(());
    }

    let (width, height) = options.size;
    app.measure(height);
    let mut terminal = Terminal::new(TestBackend::new(width, height))?;
    terminal.draw(|frame| ui::render(frame, &app))?;
    let buffer = terminal.backend().buffer().clone();
    for y in 0..buffer.area.height {
        let mut row = String::new();
        let mut skip = 0u16;
        for x in 0..buffer.area.width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let symbol = buffer[(x, y)].symbol();
            // A double-width glyph owns two cells; its filler would read as a space.
            skip = u16::from(symbol.chars().any(|c| (c as u32) > 0x2E80));
            row.push_str(symbol);
        }
        println!("{}", row.trim_end());
    }
    transport.shutdown().await;
    Ok(())
}

/// Write staged images over the cells the frame reserved for them.
///
/// Runs after `draw`, since the buffer would otherwise paint over the image. A terminal
/// with no protocol writes nothing here — the placeholder text is already in the frame.
fn draw_images(app: &App, width: u16, height: u16) -> Result<()> {
    use std::io::Write as _;

    let (placements, _) = app.image_placements(width, height);
    if placements.is_empty() {
        return Ok(());
    }
    let mut out = std::io::stdout();
    for placement in placements {
        out.write_all(placement.payload.as_bytes())?;
    }
    out.flush()?;
    Ok(())
}
