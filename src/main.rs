//! Theta — a multi-session OpenCode workspace TUI.

mod app;
mod config;
mod events;
mod fsx;
mod git;
mod highlight;
mod keys;
mod logging;
mod manager;
mod opencode;
mod panes;
mod persist;
mod session;
mod theme;
mod ui;

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    EventStream, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
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
    log: bool,
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
    --log              Write a verbose debug log (works from any directory)
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
        log: false,
        help: false,
        version: false,
    };
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--no-restore" => args.no_restore = true,
            "--log" => args.log = true,
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

    if args.log {
        let path = logging::default_path();
        match logging::init(&path) {
            Ok(()) => eprintln!("theta: --log enabled → {}", path.display()),
            Err(e) => eprintln!("theta: cannot open log file {}: {e}", path.display()),
        }
    }
    crate::tlog!(
        "=== theta {} start cwd={:?} args={:?} ===",
        env!("CARGO_PKG_VERSION"),
        std::env::current_dir().ok(),
        std::env::args().collect::<Vec<_>>()
    );

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

    install_panic_hook();
    let mut terminal = init_terminal()?;
    let result = run(&mut terminal, &mut app, &mut rx, &args).await;
    restore_terminal();

    // `/refresh`: hand the terminal to the newest binary and re-exec. The
    // OpenCode servers are left running so the new process reuses them and
    // reconnects instantly.
    if app.restart {
        exec_self()?;
    }

    // Normal exit: shut the servers down gracefully so OpenCode can
    // checkpoint its database.
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
    // 40ms frames keep the status-bar scanner as smooth as OpenCode's; the
    // app still only redraws when something changed.
    let mut tick = tokio::time::interval(Duration::from_millis(40));

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
    execute!(
        out,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
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
    let _ = execute!(
        out,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    );
    let _ = disable_raw_mode();
}

/// Restore the terminal before the default panic output so a crash never
/// leaves the user stuck in raw mode / the alternate screen.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        crate::tlog!("PANIC {info}");
        restore_terminal();
        default(info);
    }));
}

/// Replace the current process image with the theta binary, preserving CLI
/// arguments. Tries several candidate paths so a stale `current_exe` (e.g.
/// after a rebuild replaced the file) still finds a valid binary.
fn exec_self() -> Result<()> {
    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::process::CommandExt;
        use std::path::PathBuf;

        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(p) = std::env::current_exe() {
            candidates.push(p);
        }
        // The path as invoked (may be relative or a symlink).
        if let Some(arg0) = std::env::args_os().next() {
            let p = PathBuf::from(&arg0);
            if p.components().count() > 1 {
                candidates.push(p);
            }
        }
        if let Some(home) = dirs::home_dir() {
            candidates.push(home.join(".local/bin/theta"));
        }
        // Anything named `theta` on PATH.
        if let Some(paths) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&paths) {
                candidates.push(dir.join("theta"));
            }
        }

        let args: Vec<OsString> = std::env::args_os().skip(1).collect();
        let mut last_err: Option<std::io::Error> = None;
        for cand in candidates {
            if !cand.is_file() {
                continue;
            }
            let err = std::process::Command::new(&cand).args(&args).exec();
            // `exec` only returns on failure; remember and try the next.
            last_err = Some(err);
        }
        match last_err {
            Some(e) => Err(anyhow::anyhow!("refresh failed: {e}")),
            None => Err(anyhow::anyhow!("refresh failed: could not locate the theta binary")),
        }
    }
    #[cfg(not(unix))]
    {
        std::process::exit(0);
    }
}
