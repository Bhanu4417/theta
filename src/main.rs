//! Theta — a multi-session OpenCode workspace TUI.

mod app;
mod config;
mod events;
mod fsx;
mod git;
mod highlight;
mod keys;
mod manager;
mod opencode;
mod panes;
mod persist;
mod session;
mod theme;
mod ui;

use anyhow::Result;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, EventStream, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{execute, tty::IsTty};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;
use std::path::PathBuf;
use std::time::Duration;

struct Args {
    dir: Option<PathBuf>,
    no_restore: bool,
    help: bool,
    version: bool,
}

fn usage() -> &'static str {
    "Θ theta — multi-session OpenCode workspace

USAGE:
    theta [OPTIONS] [DIR]

ARGS:
    [DIR]              Open with an initial session in DIR

OPTIONS:
    --no-restore       Do not restore the last workspace
    --help             Show this help
    --version          Show version

KEYS:
    ^N new session   ^K palette   ^P files   ^⇧F project   ^F conversation
    ^B explorer      ^Space maximize   Tab focus   Alt+hjkl resize   F1 help
"
}

fn parse_args() -> Args {
    let mut args = Args {
        dir: None,
        no_restore: false,
        help: false,
        version: false,
    };
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--no-restore" => args.no_restore = true,
            "--help" | "-h" => args.help = true,
            "--version" | "-V" => args.version = true,
            _ => args.dir = Some(PathBuf::from(a)),
        }
    }
    args
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args();
    if args.help {
        print!("{}", usage());
        return Ok(());
    }
    if args.version {
        println!("theta 0.1.0");
        return Ok(());
    }
    if !stdout().is_tty() {
        eprintln!("theta: stdout is not a terminal");
        std::process::exit(1);
    }

    config::Config::save_default_if_missing()?;
    let cfg = config::Config::load()?;
    theme::set_theme(&cfg.theme);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let manager = manager::Manager::new(tx, cfg.clone());
    let initial_dir = args
        .dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let mut app = app::App::new(cfg, manager, initial_dir);

    // Warm the resume cache at boot so the session lists open instantly.
    app.preload_sessions(app.initial_dir.clone());

    let mut terminal = init_terminal()?;
    let result = run(&mut terminal, &mut app, &mut rx, &args).await;
    restore_terminal();
    // Graceful shutdown so OpenCode servers checkpoint their databases.
    app.manager.shutdown_all().await;

    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("theta: {e:#}");
            Err(e)
        }
    }
}

async fn run(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut app::App,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<events::AppEvent>,
    args: &Args,
) -> Result<()> {
    if !args.no_restore && app.cfg.ui.restore {
        app.restore_workspace();
    }
    app.dirty = true;

    let mut events = EventStream::new().fuse();
    let mut tick = tokio::time::interval(Duration::from_millis(120));

    loop {
        if app.dirty {
            terminal.draw(|f| ui::render(f, app))?;
            app.dirty = false;
        }

        tokio::select! {
            maybe_ev = events.next() => {
                match maybe_ev {
                    Some(Ok(ev)) => app.handle_term_event(ev).await,
                    Some(Err(_)) => {}
                    None => {}
                }
            }
            Some(aev) = rx.recv() => {
                app.handle_event(aev).await;
            }
            _ = tick.tick() => {
                app.on_tick().await;
            }
        }

        if app.should_quit {
            break;
        }
    }

    app.save_workspace();
    Ok(())
}

fn init_terminal() -> Result<Terminal<CrosstermBackend<std::io::Stdout>>> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    // Best-effort kitty keyboard protocol (ignored where unsupported).
    let _ = execute!(
        out,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    let backend = CrosstermBackend::new(out);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal() {
    let mut out = stdout();
    let _ = execute!(out, PopKeyboardEnhancementFlags);
    let _ = execute!(out, DisableMouseCapture, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}
