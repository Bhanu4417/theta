# AGENTS.md — rules for any agent working on Theta

This file is the contract for every agent (human-directed or autonomous) that
edits this repository. Read it fully before making changes.

## What Theta is

Theta is a **terminal client** for the OpenCode agent harness: a multi-session,
tiled-pane TUI written in Rust (ratatui + crossterm + tokio). It does **not**
talk to models or run an agent loop itself — it spawns `opencode serve` per
project directory and drives its HTTP + SSE API. See `README.md` for the user
manual and `src/` for the architecture.

**Long-term direction:** Theta is expected to evolve into an **independent
harness** (its own agent loop + tools + providers) while keeping the same UI.
Therefore: keep the harness/client seam clean. All harness communication is
already isolated in `src/manager.rs` + `src/opencode.rs`; the UI consumes
`AppEvent`s from `src/events.rs`. New features must respect that boundary so the
`opencode serve` backend can later be swapped for an in-process loop.

## Golden rules (never break these)

1. **Never delete or reorder a user's chat history.** Transcripts are precious.
2. **Never disturb a running session.** Aborting/forking/closing must be explicit.
3. **`cargo build` must succeed and `cargo test` must pass** before you consider
   a change done. If it compiles and the tests pass, it is considered working.
4. **Add or extend tests for any behavior change** (2–3 focused tests minimum).
   Prefer small, deterministic unit tests over integration tests.
5. **No panics on the event loop.** The UI must never freeze or trap the user.
   Anything potentially long-running (file I/O, highlighting, network) must be
   bounded/capped or run off the render path.
6. **Never render a blocking synchronous operation of unbounded size** (e.g.
   highlighting a whole large file). Cap it and show a truncation notice.
7. **Match the existing style.** No new dependencies without a strong reason.
8. Do not commit unless explicitly asked.

## Model to use for testing / verification

When an agent needs a model to author or verify tests, use:

- **provider:** `opencode go`
- **model:** `muse spark 1.3 contributor`

Use it for test authoring and end-to-end verification so results stay
comparable across contributors.

## Build & test

```sh
cargo build            # must be clean
cargo test             # unit tests live next to their modules (#[cfg(test)])
cargo build --release  # release artifact the user runs
```

The user runs the installed binary at `~/.local/bin/theta`. After a rebuild they
use `/refresh` (which re-execs the newest binary) or restart. The status bar
shows a build stamp `v<version> <gitshort> <HH:MM:SS>` so a refresh can be
confirmed visually.

## Architecture map

| File | Responsibility |
| --- | --- |
| `main.rs` | terminal setup, event loop (keys / SSE / tick), re-exec on `/refresh` |
| `app.rs` | all state, key routing, overlays, commands, `AppEvent` handling |
| `session.rs` | per-session transcript, input buffer, status, questions/queue |
| `manager.rs` | per-directory server lifecycle, async bridge, SSE pump |
| `opencode.rs` | typed client for the OpenCode HTTP API (lenient JSON) |
| `events.rs` | `AppEvent` — the only channel from harness → UI |
| `panes.rs` | weighted row/column tiling grid |
| `persist.rs` | workspace save/restore (`~/.local/share/theta/workspace.toml`) |
| `config.rs` | `~/.config/theta/config.toml` |
| `theme.rs` | runtime palettes + color helpers (`blend`, `spin_rgb`) |
| `highlight.rs` | syntect highlighting + OpenCode-style diff view |
| `git.rs`, `fsx.rs` | git CLI + gitignore-aware filesystem |
| `ui/` | render passes: header, panes, transcript, overlays, status bar |

Rendering is event-driven: keyboard/harness events mutate state, then a draw
pass renders from a per-session line cache (`ui/conversation.rs`) that rebuilds
only when the transcript, width, theme, or animation tick changes.

## Conventions

- Harness calls are fire-and-forget and report back via `AppEvent`; never block
  the event loop on I/O.
- Keep the `Inner` mutex in `manager.rs` short — never hold it across `await`.
- Overlays are modal; `Ctrl+C` / `Ctrl+Q` must work from **any** overlay.
- Copy actions use `Ctrl+Y` (never bare `y`, which must remain typable).
- Prefer `saturating_*` arithmetic in layout code; guard small `Rect`s.
- Long shell tools render a determinate progress bar (parse `%` from
  `metadata.output`, else ease a time-based estimate toward 95%).

## Roadmap toward an independent harness

Implemented as a client today; to become a harness without a rewrite:

1. Keep `events.rs` as the stable interface (the "protocol").
2. Add a `harness` module implementing the agent loop (provider → tool calls →
   tool results → repeat) that emits the same `AppEvent`s.
3. Providers behind one trait (start with OpenAI-compatible); tools as
   schema + handler (read/write/edit/bash/grep/glob/webfetch).
4. Own context management (token counting + compaction) and permissions.
5. Swap `manager.rs`'s `opencode serve` spawning for the local loop behind a
   config flag; the UI must not change.

Design every new feature so it works for both the current OpenCode backend and
a future in-process harness.

## Work log (recent, high-level)
- **Local backend parity push**: per-session agents + an `AgentFactory`, so the
  model picker rebuilds the provider on demand (`LocalProvider::with_factory`);
  `@file`/image attachments inline into the prompt; an `ask` tool +
  `QuestionBroker` for interactive questions; a `task` tool that runs an
  auto-approved sub-agent (no recursion); local `fork` (with pinning),
  `/undo`/`/redo` via the history tree, and graceful `share` degradation.
- **agy-style `/undo` rewind**: `RewindRow`/`RewindState` + `Overlay::Rewind`
  lists user turns with per-turn `+/-` diff stats, anchored over the chatbox.
  Selecting a turn calls `SessionTree::restore_files_after` (reverse file
  pre-images captured per turn by `tools::snapshot_paths` in the agent loop),
  moves the leaf with `LocalProvider::rewind`, drops the transcript tail, and
  restores the prompt to the input. `/redo` pops the abandoned leaf.


- Native **Anthropic (Messages API)** and **Google Gemini** providers in
  `ai/anthropic.rs` / `ai/google.rs`; `build_agent` selects by `[ai].provider`
  (with per-provider default models) and falls back to the OpenAI-compatible
  adapter. Credentials load from `~/.config/theta/keys.toml` (`/login`) with
  the `ai.api_key_env` variable as fallback (`credentials.rs`).
- Local backend now loads `AGENTS.md`/`CLAUDE.md` (cwd → root, capped) into the
  system prompt (`extensions::load_context_files`) and ships a `multiedit` tool.
- **Interactive permissions for the local loop**: `agent::permissions::Broker`
  + `AskGate` (from `behavior.local_permissions = ask|allow|deny|read-only`);
  the loop emits `PermissionAsked` and awaits the UI (`a`/`A`/`r`), auto-allows
  in headless mode.
- **Local session index + resume**: `SessionTree::list_sessions` scans sidecars;
  `/resume` lists them for the local backend and `LocalProvider::adopt` replays
  stored history into the transcript. Manual `/compact` forces a Pi-style
  compaction (`AgentLoop::force_compact`) and records a tree compaction node.



- Multi-pane workspace, slash commands, model/agent pickers, themes.
- Provider-neutral harness: `HarnessEvent` protocol, owned `SessionId`,
  task lifecycle, capability-gated `AgentProvider` adapters (PostCode adapters,
  `-Multi` field `-new`).
- Step 1 of the provider plan: **event streaming is part of the adapter
  contract** (`providers::EventPump` + `RoutedEvent`/`EventSink`); the
  OpenCode SSE transport and conversion moved into `providers/opencode`, and
  `manager.rs` supervises reconnects only. New adapters implement
  `AgentProvider` + `EventPump` and plug straight into `spawn_event_pump`.
- Step 2: **`ai/` LLM layer** — dyn-compatible `Provider` trait, OpenAI-compatible
  provider (covers OpenAI, xAI/Grok, Groq, OpenRouter, DeepSeek, Together,
  Fireworks, Ollama, LM Studio…), normalized `ProviderEvent` streaming and a
  built-in model catalog (ctx/pricing/tools).
- Step 3: **`agent/` loop + tools** — `AgentLoop` (provider → tool calls →
  results → repeat) with `read`/`write`/`edit`/`bash`/`grep`/`glob`/`webfetch`,
  a `PermissionGate`, cancellation, and context compaction (`agent::context`).
  Emits the same `HarnessEvent`s, so the UI is unchanged.
- Step 4: **backend selection** — `backend = "opencode" | "local"`; the local
  backend is `providers::local::LocalProvider` (in-process loop behind the same
  `AgentProvider` + `EventPump` contract). Server-only ops degrade gracefully.
- Step 5: **headless + skills** — `theta --print ["--json"]` runs one prompt
  through the local agent (no TUI); `extensions::Registry` discovers skills and
  prompt packs and feeds the system prompt. RPC mode and tree sessions remain.
- **Compaction mirrors Pi's `core/compaction` exactly**: `agent::context` follows
  `shouldCompact(contextTokens > window - reserveTokens)`, `findCutPoint`
  (walk back to `keepRecentTokens`, cut only at user/assistant), **split-turn**
  detection with a merged turn-prefix summary, `tokensBefore`, the initial vs
  iterative update prompt, `<conversation>`/`<previous-summary>` framing, and
  cumulative `FileOps` (`read` minus `written`/`edited`), a usage-aware
  `estimateContextTokens` (last assistant usage + tail), and rejects incomplete
  summaries (length stop / empty) instead of persisting them.
  `HarnessEvent::CompactionStarted/Finished` drive a `Compacting` session status
  ("N compacting" beside the working scanner) and a centered "conversation
  compacted" divider (`PartKind::Compaction`). Budgets in `[compaction]`.
- Compaction is now Pi-complete for the local backend: **cumulative file
  tracking** (`FileOps` → `<read-files>`/`<modified-files>`, merged across
  compactions), **iterative previous-summary reuse** (`system_prefix` excludes
  prior summaries so they are re-summarized, and the previous summary is passed
  into the summarizer), **per-model overrides**
  (`[compaction.model_overrides]`), and a tested **branch-summarization** input
  builder (`context::branch_summary_input`).
- Launch P0 ergonomics: **`@file` mentions** (`mentions::extract` → OpenCode
  file parts / inlined for local, with an inline picker), **`!`/`!!` shell
  escape**, **`Ctrl+G`/`/editor`** (`$VISITOR`/`$EDITOR` via the main loop),
  **finish notifications** (bell + `notify-send`, `behavior.notify`), and
  **`/export`** to Markdown/JSONL (`export::markdown`/`jsonl`), **long-paste
  collapsing** (`[Pasted ~N lines]`) and **clipboard image/file paste** via
  `Ctrl+V` (`[Image N]` placeholder → `data:` URL file part; `paste.rs`).
- `tree::SessionTree`: Pi-style history tree (entries with `parent`, compaction
  and branch-summary nodes, root→leaf context rebuild), JSONL persistence, and a
  `/tree` navigator overlay that jumps to earlier entries, generating an LLM
  branch summary of the abandoned work first. The local backend records each
  turn into the tree and rebuilds the prompt from the active branch.
- Queue-or-fork when an agent is busy; forked panes share history.
- `agy` CLI (Gemini) integration as an alternate one-shot backend.
- Agent questions (the `ask` tool) with a picker; permission re-fetch on
  reconnect.
- `Ctrl+Y` copy (OSC 52), mouse text selection, double-`Esc` interrupt,
  `Shift+Enter` newline, history recall with `↑`.
- OpenCode-style inline diffs (line numbers, syntax colors, tinted add/remove).
- Status-bar Knight-Rider activity scanner + per-build stamp.
- `/refresh` re-execs the new binary; tray servers reused for fast reconnect.
- Robustness: missing-dir fast-fail, panic hook restores terminal, UI can never
  be trapped by a modal, large-file viewer is capped (256 KB / 4000 lines) and
  the highlighter hard-caps at 512 KB.
