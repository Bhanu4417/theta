# AGENTS.md — architecture and conventions

This file is the contract for everyone who edits Theta, human or automated.

## What Theta is

Theta is a terminal-native, multi-agent coding workspace. A multi-session,
tiled-pane TUI in Rust (ratatui + crossterm + tokio) that runs its **own agent
harness in-process**.

It does not wrap an external agent binary and it does not talk to a model
itself: it owns the agent loop, the tools, the providers, the permission model
and context management.

```
UI (app.rs, ui/)  ←  AppEvent  ←  manager.rs  ←  providers/local  ←  ai/ (providers)
                                        └──────→ agent/ (loop, tools, compaction)
```

**The seam:** the UI never talks to a provider. Background work reports to the
UI exclusively as `AppEvent`s (`events.rs`). Keep that boundary clean.

Key modules:

| Path | Responsibility |
| --- | --- |
| `main.rs` | terminal setup, event loop, re-exec on `/refresh` |
| `app.rs` | all state, key routing, overlays, commands, `AppEvent` handling |
| `session.rs` | per-session transcript, input buffer, status, queue |
| `manager.rs` | harness bridge: async work → `AppEvent` |
| `events.rs` | `AppEvent` — the only channel from harness to UI |
| `harness/` | provider-neutral `HarnessEvent` + transcript model |
| `providers/` | `AgentProvider` + `EventPump` contract, local adapter |
| `ai/` | `Provider` trait: OpenAI-compatible, Anthropic, Gemini, Responses |
| `agent/` | agent loop, tools, named agents, context compaction |
| `tree.rs` | session history tree (JSONL sidecars) |
| `panes.rs` | weighted row/column tiling grid |
| `ui/` | render passes: header, panes, transcript, overlays, status bar |
| `mcp.rs` | stdio MCP client + dynamic `mcp__` tools |

## Golden rules

1. **Never delete or reorder a user's chat history.** Transcripts are precious.
2. **Never disturb a running session.** Aborting, forking and closing are
   always explicit.
3. **`cargo build` must succeed and `cargo test` must pass** before a change is
   considered done. If it compiles and the tests pass, it works.
4. **Add tests for any behavior change** — two or three focused, deterministic
   unit tests. Prefer those over integration tests.
5. **No panics on the event loop.** The UI must never freeze or trap the user.
   Anything long-running (file I/O, highlighting, network) is bounded or runs
   off the render path.
6. **Never render an unbounded blocking operation.** Cap it and show a
   truncation notice.
7. **Match the existing style.** No new dependencies without a strong reason.
8. Do not commit unless explicitly asked.

## Conventions

- Harness calls are fire-and-forget and report back via `AppEvent`; never block
  the event loop on I/O.
- Keep the `Inner` mutex in `providers/local/mod.rs` short — never hold it
  across an `await`.
- Overlays are modal; `Ctrl+C` and `Ctrl+Q` must work from **any** overlay.
- Copy actions use `Ctrl+Y`. Bare `y` must stay typable.
- Prefer `saturating_*` arithmetic in layout code and guard small `Rect`s.
- Long shell tools render a determinate progress bar.
- Comments here explain *why*, not *what*, and are used sparingly. Code should
  read on its own.
- The codebase is intentionally hand-formatted (compact struct literals). It is
  not rustfmt-normalized; match the surrounding style rather than reformatting.

## Build and test

```sh
cargo build            # must be warning-free
cargo test             # 189 tests, offline and deterministic
cargo clippy           # must be clean
cargo build --release  # the artifact users run
```

CI runs build, test and clippy on Linux and macOS. `RUSTFLAGS=-D warnings` is
set, so a warning is a failure.

The user runs the installed binary at `~/.local/bin/theta`. After a rebuild
they either restart it or use `/refresh`, which re-execs the newest binary. The
status bar shows a build stamp (`v<version> <gitshort> <HH:MM:SS>`) so a
refresh can be confirmed visually.

## Architecture notes

- **Adding a provider.** Implement `ai::Provider` (and `list_models` for
  discovery). Routing that needs a different wire protocol lives in its own
  module (`ai/responses.rs` is the precedent).
- **Adding a tool.** Implement `agent::tools::Tool` (schema + handler), add it
  to `default_tools()`, and decide its permission behavior in the gate.
- **A new UI-facing event.** Add a variant to `HarnessEvent` (`harness/mod.rs`)
  and handle it in `app.rs`. `harness/transcript.rs` holds the neutral
  transcript model.
- **Rendering.** A draw pass renders from a per-session line cache
  (`ui/conversation.rs`) that rebuilds only when the transcript, width, theme
  or animation tick changed. Keep it that way — it is why the UI stays smooth.
- **Context compaction** (`agent/context.rs`) keeps the prompt under the
  model's limit: a cut point that never splits a turn, a summary that folds the
  previous summary forward, and cumulative file tracking. Summaries that come
  back empty are rejected rather than persisted.
