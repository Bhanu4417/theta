# AGENTS.md — rules for any agent working on Theta

This file is the contract for every agent (human-directed or autonomous) that
edits this repository. Read it fully before making changes.

## What Theta is

Theta is a **terminal client** for the OpenCode agent harness: a multi-session,
tiled-pane TUI written in Rust (ratatui + crossterm + tokio). It does **not**
talks to models itself: it owns the agent loop, tools, providers, permissions
and context management. See `README.md` for the user manual and `src/` for the
architecture.

**Architecture:** Theta **is** an independent harness now. The agent loop lives
in `src/agent/`, providers in `src/ai/`, the local adapter in
`src/providers/local`, and the manager bridges async work to the UI as
`AppEvent`s. Keep that seam clean: the UI never talks to a provider directly.

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
| `manager.rs` | local harness bridge: async work → `AppEvent`, event pump |
| `models.rs` | shared session/model/question value types |
| `mcp.rs` | stdio MCP client + dynamic `mcp__` tools |
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
5. (Done.) The local loop replaced the external backend; the UI was unchanged.

Design every new feature against the stable `HarnessEvent` interface.

## Work log (recent, high-level)
- **Startup scroll flicker and restore fixed from root.** Previously, opening Theta
  or switching sessions caused the transcript to start at the top/middle and visibly
  flicker down to the bottom. Root causes: (1) `LocalProvider::adopt` emitted
  `TranscriptUpdate::Reset` (wiping the transcript to 0 messages) and then called
  `replay()`, streaming hundreds of individual `MessageMeta` and `Part` events
  across multiple frames; each event dirtied the UI, forcing ratatui to render
  intermediate states growing from message 1 to the bottom. (2) `restore_workspace`
  and `resume_session` only restored a 50-message cached stub rather than the
  authoritative session tree. (3) Scrolling down with mouse or PageDown incremented
  offset but didn't set `stick_bottom = true`, saving a stale offset. Fixed:
  added `TranscriptUpdate::ReplaceAll(msgs)` and `SessionTree::to_messages()` for
  atomic history hydration; `restore_workspace` and `resume_session` now preload
  from the authoritative tree on frame 0; `ReplaceAll` is an idempotent no-op if
  already hydrated; `stick_bottom` automatically heals when scrolling to the bottom.
  Tests: `replace_all_transcript_update_is_atomic_and_idempotent`,
  `tree_to_messages_reconstructs_tools_and_compactions`,
  `scrolling_down_to_bottom_restores_stick_bottom`,
  `session_tree_loads_and_converts_to_messages_for_restore`,
  `list_sessions_skips_short_fillers_and_resolves_directory`.
  Also improved `/resume` title and directory inference (skips trivial 1-word
  fillers like "so"/"hi" so substantive prompts appear as titles, and resolves
  the workspace root from snapshots).

- **Context/cost readout fixed.** `upsert_part_existing` overwrote the message
  row's `tokens`/`cost`/`completed` with the Part's `None` values, so any part
  arriving after the usage update blanked the footer's `ctx`/`$` readout.
  Metadata is now merged (only set when present), and `recompute_metrics` picks
  the most recent assistant row *that has tokens* so the readout can't blink to
  0. Test: `a_part_does_not_wipe_recorded_tokens_or_cost`.

- **No mid-task stop, no streaming flicker, scroll restored.** (1) The local
  loop's turn cap defaulted low (24 in `AgentLoop::new`) and stopped real tasks
  with "(stopped after N tool rounds…)"; it is now unlimited by default
  (`[ai] max_turns = 0`, matching OpenCode/Pi — Ctrl+C interrupts), with a
  positive value available as an explicit safety stop. (2) Streamed **assistant**
  text was re-wrapped through the markdown renderer on every token, so a fast
  model flickered; in-flight text (messages after the last user prompt) is now
  hidden until the turn completes, and the reasoning preview is gone — only the
  stable spinner + timer show while working. (3) Scroll position is persisted as
  it changes (`persist_scroll_if_changed` in `on_tick`, throttled) and restored
  on `--refresh`/restart, so a pane reopens where you left it.

- **`ask` works under permissive permissions; parallel split-turn compaction.**
  The question broker was only wired when `local_permissions = "ask"`, so with
  the new `allow` default every `ask` call failed (`ask: no interactive session
  available` — the red `✗ ask`). `local_gates` now returns gates for every
  interactive session regardless of mode (headless still gets none); the gate
  itself is chosen separately. Split-turn compaction ran its history and
  turn-prefix summaries **sequentially** (~2× the latency); they now run
  concurrently via `tokio::join!`. Test:
  `interactive_sessions_always_wire_a_question_broker`. Verified in a real TUI:
  `ask` overlay renders, and with `ask` mode the `[a] allow once / A always / r
  reject` bash prompt allows the command.

- **Local transcript ids + replay fixed (the "reply only after restart" bug).**
  Three linked defects: (1) assistant message ids were `local-{turn}` and
  reused every turn *and* every launch, and `upsert_part` matches by id — so a
  new reply overwrote an older message (often near the top) instead of
  appending; ids are now `local-{run}-{turn}` with a unique run id
  (`agent::local_run_id`), and optimistic user ids embed their creation time.
  (2) `upsert_message_meta` only updated existing rows, so replayed history had
  no role row and the part handler defaulted every message to `Assistant` (user
  prompts lost their `Θ` marker) — it now inserts the row; the handler adopts
  FIFO *before* upserting to avoid duplicates. (3) restoring a workspace stacked
  `replay`ed turns on the hydrated cache; a new `TranscriptUpdate::Reset` is
  emitted by `LocalProvider::adopt` so the tree is authoritative. Tests:
  `local_message_ids_are_unique_across_turns`,
  `message_meta_inserts_a_row_so_parts_keep_their_role`,
  `non_streamed_answer_still_reaches_the_transcript`. Verified in a real TUI
  (tmux): restored history renders with correct roles, no duplicates, and the
  reply appears live.

- **Replies can't be invisible.** The transcript only received assistant text
  from streamed `TextDelta`s; a provider that assembled `turn.text` without
  emitting deltas left the UI blank while the answer sat in history/tree — it
  only appeared after a restart. The loop now emits the final text whenever no
  delta was seen. (Regression test: `non_streamed_answer_still_reaches_the_transcript`.)

- **Reasoning models no longer go silent.** `muse-spark`/`gpt-5`/`grok-4`
  default to `reasoning.effort = "high"`, which can consume the entire
  `max_output_tokens` in hidden reasoning and return **no text**. The Responses
  adapter now handles `response.incomplete` (reports `Length`), captures text
  from `output_text.done`/message items (not just deltas), and errors on an
  empty incomplete turn; the agent loop renders a "(no output …)" notice instead
  of idly finishing. `ChatRequest.reasoning_effort` maps to
  `reasoning:{effort}` on Responses only, configured via `[ai]
  reasoning_effort`; compaction summarizes at `low` (muse 4.4s, 342 reasoning
  tokens). `--check-ai` uses a 1k cap + `low` (96 tokens always came back
  empty for muse).

- **OpenCode-style tool rows + stable thinking.** The local loop no longer sets
  `ToolInfo.title` to the bare tool name, so `display_title()` derives what the
  call actually did from its args (`Reading src/app.rs`, `Searching "foo"`,
  `$ cmd`) instead of every row reading just `read`/`grep`. Reasoning deltas are
  accumulated before emit (the part is keyed by id, so emitting only the delta
  made the UI replace the text every token — the "thinking text changes too
  fast" flicker) and the span is bracketed with `start`/`end` for a real timer.

- **Fast compaction for every model.** The summary output is hard-capped
  (`context::SUMMARY_MAX_TOKENS_CAP`, 4k — was `0.8 * reserve ≈ 13k`), so
  reasoning models no longer grind on `/compact`; a length-truncated but
  non-empty summary is kept instead of failing the compaction. `[compaction]
  model` selects a dedicated fast/cheap summarizer (`AgentLoop::
  with_compaction_model`), falling back to the session model if it can't be
  built.

- **`ai.timeout_secs` is now enforced.** Every provider request carries a total
  timeout (`Provider::set_timeout`, applied in `make_provider`), so a stalled
  gateway can no longer hang a turn or `/compact` forever (default 300s, `0`
  disables).

- **OpenCode-style permissive defaults.** `behavior.local_permissions` now
  defaults to `"allow"` (was `"ask"`): tools — including `bash` and edits — run
  without prompts, matching OpenCode's defaults. `ask` remains opt-in and
  auto-allows in-project reads; `plan`/`explore` stay `read-only`. `A`
  ("always") still flips `auto_approve_permissions` for the ask mode.

- **Tool-call/result integrity (Pi-style).** Interrupting during a tool call used
  to leave an assistant `tool_calls` with no matching output; every later
  request then 400'd (`assistant message with 'tool_calls' must be followed by
  tool messages`). The loop now synthesizes a tool output for every pending call
  on cancel, and `context::repair_tool_calls` heals history (adds missing
  outputs, drops orphans) before each request — so existing broken sessions
  recover without deleting history.

- **OpenCode-style read permissions.** `AskGate` now auto-allows pure reads
  (`read`/`grep`/`glob`/`webfetch`) whose target resolves inside the session's
  working directory; only mutations, `bash`, and out-of-tree reads prompt. The
  gate signature is `check(tool, input, cwd)`. `/logs` overlays the debug log
  in-TUI.

- **`theta --log` views, `--log --run` records, `/logs` tails in-TUI.** `--log`
  alone opens/tails the newest log (`less +F`, else `tail -f`); `--log --run`
  (or `--log --print`) runs with logging enabled. The agent loop logs every
  provider call as `REQ provider=… model=… msgs/tools` and `RESP model=…
  ms/tokens`, so the request→model routing is visible. `/logs` opens a live
  overlay tailing the same file (`f` follow, `↑/↓` scroll). `--log` no longer
  silently starts the TUI.
- **"allow once" / "always" permissions.** The prompt keeps `[a] allow once`,
  `[A] always`, `[r] reject`; choosing **always** now persists
  `behavior.auto_approve_permissions = true` so the per-tool access prompts stop
  for good instead of re-appearing on every call.

- **OpenAI Responses API adapter** (`ai/responses.rs`). OpenCode Zen/Go serve
  some models (`muse-spark-*`, `gpt-5*`, `grok-4*`) only via `/responses`;
  `ai::provider_for_model` routes those to the new streaming adapter (SSE
  `response.output_text.delta` / `response.output_item.done` function calls /
  `response.completed` usage), while everything else keeps `/chat/completions`.
  Verified live: `theta --check-ai` with model `muse-spark-1.3-contributor` and
  `deepseek-v4.1-flash` both return OK on `opencode-go`.

- **`/model` discovers live models from logged-in providers.** `ai::discovery`
  owns the provider table (shared with `/login`) and `discover_models` queries
  exactly the configured providers — those with a stored key, the active
  provider (env fallback), the active custom endpoint, and local Ollama only
  when it is the active provider — via
  `Provider::list_models` — `GET /models` for OpenAI-compatible gateways
  (including OpenCode Zen/Go), `/v1/models` for Anthropic, `/v1beta/models` for
  Gemini. Results are **persisted per provider** in
  `~/.cache/theta/models.json` (keyed by a key fingerprint), so `/model` does
  not re-fetch; `/login` invalidates only the provider whose key changed. They
  merge with the built-in catalog (fallback), and the picker refreshes on open
  and after `/login`. The agent factory clears `base_url` when switching to a
  named provider so presets win over a stale custom endpoint. The API-key
  prompt shows no model — models appear in `/model` after the fetch.

- **`/login` mirrors the OpenCode CLI provider list.** `opencode` (OpenCode Zen,
  `https://opencode.ai/zen/v1`) and `opencode-go` (`…/zen/go/v1`) lead the
  picker, followed by anthropic/openai/google/xai/groq/deepseek/openrouter/
  together/fireworks/mistral/cerebras/perplexity/ollama. Choosing a provider
  stores its key and **activates it immediately** (provider + default model +
  `ai.api_key_env`, persisted, via `Manager::set_ai`/`reload_credentials`), so
  no restart is needed. Base URLs live in `OpenAiCompat::preset` (the zen hosts
  auto-add `x-opencode-session`); `/login <provider> <key>` does the same
  non-interactively. The custom-endpoint flow still stores `[ai].base_url`.


- **OpenCode removed — Theta is the harness.** Deleted `opencode.rs`, the
  `providers/opencode` SSE adapter, the `[opencode]`/`backend` config, the agy
  one-shot path and their events/overlays. `manager.rs` is now a local-only
  bridge; shared value types live in `models.rs`. Users authenticate with the
  interactive `/login` picker (keys.toml). `--check-ai`, MCP, agents, vision and
  retries all run on Theta's own loop.


- **Pi-level parity push**: named agents (`agent::agents` — build/plan/general/
  explore with per-agent tool sets, prompts, permission presets and delegation);
  the `task` tool takes a `subagent_type`; provider retry/backoff
  (`ai::stream_with_retry`, `is_retryable_provider_error`, `[ai] max_retries`);
  **vision** via `ChatMessage.images`/`ImagePart` mapped to OpenAI `image_url`,
  Anthropic `image` blocks and Gemini `inlineData`; **MCP** stdio client
  (`src/mcp.rs`) with dynamic `mcp__<server>__<tool>` tools from `[mcp.*]`;
  `THETA_AI_*` env overrides and `theta --check-ai`.
- Live-verified the local loop end to end against an OpenAI-compatible gateway
  (`https://opencode.ai/zen/go/v1`, model `deepseek-v4.1-flash`): `--check-ai`
  returns OK and a headless turn executes the `bash` tool for real. The zen
  gateway needs an `x-opencode-session` header, added automatically for that
  host. Anthropic/Gemini native providers remain unit-tested only (no keys).


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
