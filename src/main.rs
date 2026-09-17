#[allow(dead_code)]
mod agent;
#[allow(dead_code)]
mod ai;
mod app;
mod config;
mod credentials;
mod demo;
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
mod models;
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
    run: bool,
    demo: bool,
    help: bool,
    version: bool,
    print: bool,
    json: bool,
    prompt: Vec<String>,
    check_ai: bool,
    model: Option<String>,
    theme: Option<String>,
}

fn usage() -> &'static str {
    "Θ theta — terminal coding agent (local harness, multi-session)

USAGE:
    theta [OPTIONS] [DIR]

ARGS:
    [DIR]              Open with an initial session in DIR

OPTIONS:
    --model NAME       Use NAME instead of the configured model
    --no-restore       Do not restore the last workspace
    --log              Open/tail the newest debug log (request → provider/model)
    --run              Start the TUI (with --log: record to the log file)
    --demo             Run interactive multi-agent demo scenario
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
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from(argv: impl IntoIterator<Item = String>) -> Args {
    let mut args = Args {
        dir: None,
        no_restore: false,
        log: false,
        run: false,
        demo: false,
        help: false,
        version: false,
        print: false,
        json: false,
        prompt: Vec::new(),
        check_ai: false,
        model: None,
        theme: None,
    };
    let mut positional: Vec<String> = Vec::new();
    let mut argv = argv.into_iter();
    while let Some(a) = argv.next() {
        match a.as_str() {
            "--no-restore" => args.no_restore = true,
            "--log" => args.log = true,
            "--run" => args.run = true,
            "--demo" => args.demo = true,
            "--help" | "-h" => args.help = true,
            "--version" | "-V" => args.version = true,
            "--print" | "-p" => args.print = true,
            "--json" => args.json = true,
            "--check-ai" => args.check_ai = true,
            // Takes the next argument, or the part after `--model=`.
            "--model" => args.model = argv.next(),
            "--theme" => args.theme = argv.next(),
            _ => {
                if let Some(m) = a.strip_prefix("--model=") {
                    args.model = Some(m.to_string());
                } else if let Some(t) = a.strip_prefix("--theme=") {
                    args.theme = Some(t.to_string());
                } else {
                    positional.push(a);
                }
            }
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
        // From Cargo.toml, so it can never drift from the released version. It
        // was hardcoded, which meant `--version` (used by the release smoke
        // test and the installers' post-install check) reported a stale number
        // no matter which version was actually built.
        println!("theta {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    config::Config::save_default_if_missing()?;
    let mut cfg = config::Config::load()?;
    if let Some(model) = args.model.as_ref().filter(|m| !m.trim().is_empty()) {
        cfg.ai.model = model.trim().to_string();
    }
    if let Some(t) = args.theme.as_ref().filter(|t| !t.trim().is_empty()) {
        cfg.theme = t.trim().to_string();
    }
    theme::set_theme(&cfg.theme);

    let recording = args.run || args.print || args.json || args.check_ai || args.demo;
    if args.log && !recording {
        return view_log();
    }
    if args.log {
        let path = logging::default_path();
        match logging::init(&path) {
            Ok(()) => eprintln!("theta: logging to {}", path.display()),
            Err(e) => eprintln!("theta: cannot open log file {}: {e}", path.display()),
        }
    }

    if args.check_ai {
        return run_check_ai(cfg, args.prompt).await;
    }

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

    crate::tlog!(
        "=== theta {} start cwd={:?} args={:?} ===",
        env!("CARGO_PKG_VERSION"),
        std::env::current_dir().ok(),
        std::env::args().collect::<Vec<_>>()
    );

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let manager = manager::Manager::new(tx.clone(), cfg.clone());
    let initial_dir = args
        .dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let mut app = app::App::new(cfg, manager, initial_dir);

    if args.demo {
        app.is_demo = true;
        demo::setup_demo_app(&mut app, args.theme.as_deref());
        let demo_tx = tx.clone();
        tokio::spawn(async move {
            demo::run_demo_loop(demo_tx).await;
        });
    } else {
        if !app.provider_configured() {
            app.flash("no API key yet — run /login to add one");
        }
        app.preload_sessions(app.initial_dir.clone());
    }

    install_panic_hook();
    let mut terminal = init_terminal()?;
    let result = run(&mut terminal, &mut app, &mut rx, &args).await;
    restore_terminal();

    if app.restart {
        exec_self()?;
    }

    app.manager.shutdown_all().await;

    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("theta: {e:#}");
            Err(e)
        }
    }
}

fn view_log() -> Result<()> {
    let dir = logging::logs_dir();
    let Some(path) = logging::newest_log(&dir) else {
        eprintln!("theta: no logs in {}", dir.display());
        eprintln!("theta: record one with `theta --log --run`");
        return Ok(());
    };
    println!("theta: {}  (q to quit)", path.display());
    if std::process::Command::new("less").arg("+F").arg(&path).status().is_ok() {
        return Ok(());
    }
    if std::process::Command::new("tail")
        .args(["-n", "300", "-f"])
        .arg(&path)
        .status()
        .is_ok()
    {
        return Ok(());
    }
    print!("{}", std::fs::read_to_string(&path).unwrap_or_default());
    Ok(())
}

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
            max_tokens: Some(1_024),
            reasoning_effort: Some("low".into()),
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
    if !args.demo && !args.no_restore && app.cfg.ui.restore {
        app.restore_workspace();
    }
    app.preload_known_dirs();
    app.dirty = true;

    let mut events = EventStream::new().fuse();
    let mut tick = tokio::time::interval(Duration::from_millis(40));

    // At most one frame per interval. A streamed reply marks the app dirty on
    // every token, which can be hundreds of times a second; drawing each one
    // re-wraps text faster than it can be read and makes the pane flicker.
    // Coalescing to a frame rate keeps it smooth, and typing still feels
    // instant because input preempts the wait.
    const MIN_RENDER_INTERVAL: Duration = Duration::from_millis(16);
    let mut last_draw = std::time::Instant::now();

    loop {
        if app.needs_clear {
            // A resize can leave cells from the previous, wider layout behind.
            // Clearing the physical screen (ratatui only re-draws changed
            // cells) prevents those patches appearing over the text.
            terminal.clear()?;
            app.needs_clear = false;
            app.dirty = true;
            app.urgent = true;
        }

        if app.dirty {
            let due = app.urgent || last_draw.elapsed() >= MIN_RENDER_INTERVAL;
            if due {
                terminal.draw(|f| ui::render(f, app))?;
                app.dirty = false;
                app.urgent = false;
                last_draw = std::time::Instant::now();
            }
        }

        // When a frame is pending but throttled, wake exactly when it is due
        // rather than waiting for the next tick.
        let frame_due = app.dirty.then(|| last_draw + MIN_RENDER_INTERVAL);

        tokio::select! {
            maybe_ev = events.next() => {
                match maybe_ev {
                    Some(Ok(ev)) => {
                        // Input is latency-sensitive, so it must not wait behind
                        // a coalesced frame.
                        app.urgent = true;
                        app.handle_term_event(ev).await;
                    }
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
            _ = async {
                match frame_due {
                    Some(at) => tokio::time::sleep_until(at.into()).await,
                    // Nothing pending: never resolve, so this branch is inert.
                    None => std::future::pending::<()>().await,
                }
            } => {}
        }

        if app.should_quit {
            break;
        }

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

    if !args.demo {
        app.save_workspace();
    }
    Ok(())
}

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

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        crate::tlog!("PANIC {info}");
        restore_terminal();
        default(info);
    }));
}

fn exec_self() -> Result<()> {
    use std::ffi::OsString;
    use std::path::PathBuf;

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::current_exe() {
        candidates.push(p);
    }
    if let Some(arg0) = std::env::args_os().next() {
        let p = PathBuf::from(&arg0);
        if p.components().count() > 1 {
            candidates.push(p);
        }
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/bin/theta"));
    }
    let exe = if cfg!(windows) { "theta.exe" } else { "theta" };
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            candidates.push(dir.join(exe));
        }
    }

    let args: Vec<OsString> = std::env::args_os().skip(1).collect();

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let mut last_err: Option<std::io::Error> = None;
        for cand in candidates {
            if !cand.is_file() {
                continue;
            }
            last_err = Some(std::process::Command::new(&cand).args(&args).exec());
        }
        match last_err {
            Some(e) => Err(anyhow::anyhow!("refresh failed: {e}")),
            None => Err(anyhow::anyhow!("refresh failed: could not locate the theta binary")),
        }
    }

    #[cfg(not(unix))]
    {
        // `exec` is unix-only. Spawn the new build and stay alive until it
        // exits, so the restarted TUI owns the terminal instead of racing a
        // shell prompt for it.
        for cand in candidates {
            if !cand.is_file() {
                continue;
            }
            if let Ok(mut child) = std::process::Command::new(&cand).args(&args).spawn() {
                let status = child.wait();
                std::process::exit(status.ok().and_then(|s| s.code()).unwrap_or(0));
            }
        }
        Err(anyhow::anyhow!("refresh failed: could not locate the theta binary"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(a: &[&str]) -> Args {
        parse_args_from(a.iter().map(|s| s.to_string()))
    }

    #[test]
    fn model_flag_takes_next_argument() {
        let a = parse(&["--model", "union-alpha", "hi"]);
        assert_eq!(a.model.as_deref(), Some("union-alpha"));
        // The model name is consumed, not mistaken for a directory or prompt.
        assert_eq!(a.dir, Some(PathBuf::from("hi")));
        assert!(a.prompt.is_empty());
    }

    #[test]
    fn model_flag_accepts_equals_form_with_print() {
        let a = parse(&["--print", "--model=union-alpha", "hi", "there"]);
        assert_eq!(a.model.as_deref(), Some("union-alpha"));
        assert!(a.print);
        assert_eq!(a.prompt, vec!["hi".to_string(), "there".to_string()]);
    }

    #[test]
    fn model_flag_absent_leaves_model_none() {
        let a = parse(&["--print", "hi"]);
        assert_eq!(a.model, None);
        assert_eq!(a.prompt, vec!["hi".to_string()]);
    }

    #[test]
    fn demo_flag_parsed() {
        let a = parse(&["--demo"]);
        assert!(a.demo);
    }

    #[test]
    fn theme_flag_parsed() {
        let a = parse(&["--theme", "theta-night"]);
        assert_eq!(a.theme.as_deref(), Some("theta-night"));

        let a2 = parse(&["--theme=rose-fjord"]);
        assert_eq!(a2.theme.as_deref(), Some("rose-fjord"));
    }
}
