# Θ Theta

**A terminal-native multi-agent workspace for [OpenCode](https://opencode.ai).**

Run multiple OpenCode sessions simultaneously in one terminal and view them as
beautifully tiled, resizable panes — Tokyo at night, for your coding agents.

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ Θ  THETA                                            ~/projects/my-app    │
├───────────────────────────────┬──────────────────────────────────────────┤
│ Θ authentication          ○   │ Θ database                        ⠋      │
│                               │                                          │
│ You                           │ You                                      │
│ Fix the JWT middleware.       │ Refactor the database layer.             │
│                               │                                          │
│ ✓ Reading src/auth.rs         │ Θ Refactoring…                           │
│ ✓ Searching "validate_token"  │ ◦ Editing src/db/schema.rs               │
│                               │ ◦ Running cargo test                     │
├───────────────────────────────┼──────────────────────────────────────────┤
│ Θ Ask this agent…             │ Θ Ask this agent…                        │
└───────────────────────────────┴──────────────────────────────────────────┘
```

## Building

```sh
cargo build --release
# binary: target/release/theta
```

Requires Rust 1.80+ and the `opencode` CLI on your `PATH`.

Install it so you can just type `theta` anywhere:

```sh
cargo build --release && install -Dm755 target/release/theta ~/.local/bin/theta
```

## Running

```sh
theta                  # restore last workspace (or show empty state)
theta ~/projects/app   # open with the workspace rooted at DIR
theta --no-restore     # skip workspace restore
```

Theta spawns one headless `opencode serve` process per project directory
(reusing a healthy one if it is already there) and talks to it over its real
HTTP + SSE API: session creation, `prompt_async`, live
`message.part.updated` streaming, tool state, permission requests,
`session.idle`/`session.error`, and history replay on restore.

## Slash commands

Type `/` in any session input for the command menu (filter by typing,
`↑/↓` to choose, `Enter` to run, `Tab` to complete):

| Command | Action |
| --- | --- |
| `/model [provider/model]` | Fetch and switch the session's model (searchable picker) |
| `/agent [name]` | Switch agent (build, plan, …) |
| `/new` | New session |
| `/sessions` | Jump to another open session |
| `/clear` | Clear the transcript view (server history kept) |
| `/compact` | Summarize the conversation |
| `/undo` / `/redo` | Revert / re-apply the last message |
| `/share` / `/unshare` | Share the session and get a URL |
| `/init [focus]` | Guided AGENTS.md setup (server command) |
| `/help` · `/quit` | Keys · quit |

Custom commands defined in the project's OpenCode config are fetched from the
server and appear in the same menu automatically. The chosen model and agent
are used for subsequent prompts in that session and shown in the status bar.

## Keys

| Key | Action |
| --- | --- |
| `^N` | New session (name + directory, or resume a recent one) |
| `^R` | Resume a previous OpenCode session (searchable list) |
| `^W` | Close session |
| `^K` | Command palette |
| `^T` | Change tiling (auto grid / rows / columns) |
| `^Space` | Maximize / restore pane |
| `^O` | Switch session (list of open panes) |
| `Tab` / `S-Tab` | Cycle pane focus |
| `Alt+Arrows` | Focus pane in a direction |
| `Alt+h j k l` | Resize pane left / down / up / right |
| `Alt+⇧h/j/k/l` | Move pane in a direction (Hyprland-style) |
| `Alt+1..9` | Focus nth pane |
| `^P` | Search files (server-side, gitignore-aware) |
| `^⇧F` (or `Alt+⇧F`) | Search project content |
| `^F` | Search current conversation |
| `^B` | Toggle file explorer panel |
| `^C` | Interrupt the focused agent |
| `PgUp` / `PgDn` / wheel | Scroll transcript (`End` re-follows) |
| `↑`/`↓`, `Enter` | Select a tool entry / expand–collapse it |
| `d` / `o` | Diff / open file of the selected tool |
| `a` / `A` / `r` | Permission: allow once / always / reject |
| `^Q` | Quit (confirms while agents are working) |
| `F1` | Help |

## Layout

Panes auto-tile by terminal aspect: 2 → side-by-side, 3 → two over one,
4 → 2×2, 6 → 3×2, 8 → 4×2 — or switch the whole workspace to **rows** or
**columns** with `^T` and move panes around with `Alt+Shift+hjkl`. Adjacent
panes share separator lines. When the terminal is too small for the grid,
the focused session stays full-screen and usable. Layouts, weights, and open sessions persist to
`~/.local/share/theta/workspace.toml` and are restored on the next launch —
including full transcript history replay from the OpenCode server. The
new-session dialog skips model picking (the server default is used; change
it any time with `/model`) and lists recent server sessions to resume.

## Usage metrics

Like the OpenCode TUI, Theta surfaces live usage: per-session prompt context
size (`ctx 4%` when the model's context limit is known, raw tokens otherwise)
and cost on the pane separator, plus the whole-workspace cost in the status
bar. Thinking shows an animated elapsed timer while the model reasons and a
quiet `✓ thought for 4.7s` marker afterwards.

## Configuration

`~/.config/theta/config.toml` (written with defaults on first run):

```toml
[opencode]
binary = "opencode"      # server binary to launch
port_base = 4310         # per-directory servers use ports [base, base+1500)

[ui]
restore = true           # restore last workspace on startup
explorer_width = 32

[behavior]
auto_approve_permissions = false
confirm_quit = true
```

## Architecture

```text
src/
├── main.rs            terminal setup + event loop (select over keys/SSE/tick)
├── app.rs             state, key routing, commands, overlay state
├── events.rs          AppEvent: manager → UI channel
├── panes.rs           weighted row/column grid: auto-tile, resize, swap
├── session.rs         per-session transcript, input, status, adoption
├── manager.rs         per-directory opencode serve lifecycle + SSE pumps
├── opencode.rs        typed client for the OpenCode HTTP API
├── git.rs             async git CLI (branch/status/log/diff), cached
├── fsx.rs             gitignore-aware listing + local search fallback
├── highlight.rs       syntect with an embedded Tokyo Night theme
├── config.rs          ~/.config/theta/config.toml
├── persist.rs         workspace save/restore
└── ui/                header, panes, conversation cache, overlays
```

Rendering is event-driven: keyboard/OpenCode events mutate state, the draw
pass renders from a per-session line cache that rebuilds only when the
transcript, width, or spinner tick changes.
