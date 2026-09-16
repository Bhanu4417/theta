//! Theta — a multi-session OpenCode workspace TUI.

// The ai/agent/extensions layers are a reusable library surface; not every
// entry point is wired into the TUI yet.
#[allow(dead_code)]
mod agent;
#[allow(dead_code)]
mod ai;
mod app;
mod config;
mod credentials;
mod events;
#[allow(dead_code)]
mod export;
#[allow(dead_code)]
mod extensions;
mod fsx;
mod git;
mod harness;
mod highlight;
mod keys;
mod logging;
mod mcp;
mod manager;
mod mentions;
mod opencode;
mod panes;
mod paste;
mod persist;
mod providers;
mod session;
mod theme;
mod tree;
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
    /// Headless: run one turn and print the reply, no TUI.
    print: bool,
    /// Headless: emit neutral harness events as JSON lines.
    json: bool,
    /// Prompt / positional arguments.
    prompt: Vec<String>,
    /// Live-verify LLM providers (positional args are provider ids).
    check_ai: bool,
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
    -p, --print        Run one prompt headlessly and print the reply
    --json             With --print, emit events as JSON lines
    --check-ai [PROV…] Live-verify provider credentials (default: [ai].provider)
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
        print: false,
        json: false,
        prompt: Vec::new(),
        check_ai: false,
    };
    let mut positional: Vec<String> = Vec::new();
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--no-restore" => args.no_restore = true,
            "--log" => args.log = true,
            "--help" | "-h" => args.help = true,
            "--version" | "-V" => args.version = true,
            "--print" | "-p" => args.print = true,
            "--json" => args.json = true,
            "--check-ai" => args.check_ai = true,
            _ => positional.push(a),
        }
    }
    if args.print || args.json || args.check_ai {
        args.prompt = positional;
    } else {
        args.dir = positional.into_iter().next().map(PathBuf::from);
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

    config::Config::save_default_if_missing()?;
    let cfg = config::Config::load()?;
    theme::set_theme(&cfg.theme);

    if args.check_ai {
        return run_check_ai(cfg, args.prompt).await;
    }

    // Headless modes do not need a TTY (print / JSON event stream).
    if args.print || args.json {
        let prompt = args.prompt.join(" ");
        if prompt.trim().is_empty() {
            eprintln!("theta: --print requires a prompt");
            std::process::exit(2);
        }
        return run_headless(cfg, prompt, args.json).await;
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

    // Normal exit: with `keep_alive` (default) the servers stay up so the next
    // launch reuses them instantly; otherwise shut them down gracefully so
    // OpenCode can checkpoint its database.
    if !app.cfg.opencode.keep_alive {
        app.manager.shutdown_all().await;
    }

    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("theta: {e:#}");
            Err(e)
        }
    }
}

/// Live-verify providers by sending a tiny request to each and reporting the
/// result. Missing credentials are skipped, not failed.
async fn run_check_ai(cfg: config::Config, providers: Vec<String>) -> Result<()> {
    let list = if providers.is_empty() {
        vec![cfg.ai.provider.clone()]
    } else {
        providers
    };
    for id in list {
        let mut c = cfg.clone();
        c.ai.provider = id.clone();
        if c.ai.model.trim().is_empty() {
            c.ai.model = manager::default_model_for(&id.to_ascii_lowercase()).to_string();
        }
        if c.ai.model.is_empty() {
            c.ai.model = "gpt-4o".to_string();
        }
        let creds = credentials::Credentials::load();
        let pid = id.to_ascii_lowercase();
        let has_local = !c.ai.base_url.trim().is_empty();
        if !has_local && creds.resolve(&pid, &c.ai.api_key_env).is_none() {
            println!("{id}: skipped (no credentials — set a key via /login or ${})", c.ai.api_key_env);
            continue;
        }
        let provider = match manager::make_provider(&c) {
            Ok(p) => p,
            Err(e) => {
                println!("{id}: unsupported — {e}");
                continue;
            }
        };
        let req = ai::ChatRequest {
            model: c.ai.model.clone(),
            messages: vec![ai::ChatMessage::user("Reply with the single word OK.")],
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(96),
        };
        let start = std::time::Instant::now();
        let mut got = String::new();
        let mut reasoning = String::new();
        let mut on_event = |ev: ai::ProviderEvent| match ev {
            ai::ProviderEvent::TextDelta(t) => got.push_str(&t),
            ai::ProviderEvent::ReasoningDelta(t) => reasoning.push_str(&t),
            _ => {}
        };
        match ai::stream_with_retry(provider.as_ref(), req, &mut on_event, c.ai.max_retries, c.ai.retry_base_ms).await {
            Ok(turn) => {
                let mut text = if turn.text.trim().is_empty() { got } else { turn.text };
                if text.trim().is_empty() && !reasoning.trim().is_empty() {
                    text = format!("(reasoning-only) {}", reasoning);
                }
                println!(
                    "{id}: OK ✓ (model {}, {} ms) — {:?}",
                    c.ai.model,
                    start.elapsed().as_millis(),
                    text.trim().chars().take(60).collect::<String>()
                );
            }
            Err(e) => println!("{id}: FAIL (model {}) — {e}", c.ai.model),
        }
    }
    Ok(())
}

/// Run one prompt through the local agent and print the result. `--json`
/// streams every provider-neutral event as a JSON line; otherwise only the
/// final assistant text is printed.
async fn run_headless(cfg: config::Config, prompt: String, json: bool) -> Result<()> {
    let (agent, _broker) = manager::build_agent(&cfg, false)?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut history = Vec::new();
    let final_text = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let sink = final_text.clone();
    let mut emit = move |ev: harness::HarnessEvent| {
        if json {
            if let Ok(s) = serde_json::to_string(&ev) {
                println!("{s}");
            }
        }
        if let harness::HarnessEvent::Transcript(harness::TranscriptUpdate::Part(p)) = &ev {
            if let harness::transcript::PartKind::Text { text, synthetic: false } = &p.kind {
                *sink.lock().unwrap() = text.clone();
            }
        }
    };
    agent.run_turn(&mut history, &prompt, &cwd, &mut emit).await?;
    if !json {
        println!("{}", final_text.lock().unwrap());
    }
    Ok(())
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
    // Pull session lists for every folder we know about (through one server).
    app.preload_known_dirs();
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

        // Suspend the TUI to compose a prompt in `$EDITOR` (Ctrl+G / /editor).
        if let Some((sid, initial)) = app.take_pending_editor() {
            restore_terminal();
            let edited = edit_in_external_editor(&initial);
            *terminal = init_terminal()?;
            terminal.clear()?;
            app.dirty = true;
            if let Some(text) = edited {
                app.set_input(sid, text);
            }
        }
    }

    app.save_workspace();
    Ok(())
}

/// Run the user's editor on a temp file and return the edited text.
/// `$VISUAL`, then `$EDITOR`, then a sensible default.
fn edit_in_external_editor(initial: &str) -> Option<String> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("EDITOR").ok().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "notepad".into()
            } else {
                "vi".into()
            }
        });
    let path = std::env::temp_dir().join(format!("theta-prompt-{}.md", std::process::id()));
    if std::fs::write(&path, initial).is_err() {
        return None;
    }
    let status = std::process::Command::new(&editor).arg(&path).status();
    let text = std::fs::read_to_string(&path).ok();
    let _ = std::fs::remove_file(&path);
    match status {
        Ok(s) if s.success() => text,
        _ => None,
    }
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
