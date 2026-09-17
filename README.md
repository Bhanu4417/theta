<div align="center">

<img src="assets/logo.svg" width="150" alt="Theta">

# Θ Theta

**Many coding agents. One terminal. Zero ceremony.**

A tiled, multi-session workspace that runs its own agent harness in-process —
no server to start, no daemon to babysit, no wrapper around somebody else's CLI.

[![CI](https://github.com/Bhanu4417/theta/actions/workflows/ci.yml/badge.svg)](https://github.com/Bhanu4417/theta/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e0dbce.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-e0dbce.svg)](https://www.rust-lang.org)
[![Tests](https://img.shields.io/badge/tests-259%20passing-e0dbce.svg)](#testing)

</div>

---

```
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
│                               │                                          │
│ build · deepseek-v4.1-flash   │ plan · muse-spark-1.3-contributor        │
│ ctx 4% · $0.0012 · ~/app      │ ctx 11% · $0.0004 · ~/app                │
├───────────────────────────────┼──────────────────────────────────────────┤
│ Θ Ask this agent…             │ Θ Ask this agent…                        │
└───────────────────────────────┴──────────────────────────────────────────┘
 1 session · 1 working ⣾   main ↑2 · +125 −8      Ctrl+K Commands  Ctrl+N New  Ctrl+Q Quit
```

## Why Theta

**It's not a wrapper — it's the harness.** The agent loop, tool execution,
permission gating, context compaction and provider layer all live in this
binary. That has consequences you feel every day:

| | |
| --- | --- |
| **Opens instantly** | ~2 ms to process start, ~50 ms to a drawn UI. Nothing on the boot path touches the network — credentials resolve lazily, only when you send a prompt. |
| **Nothing to install** | No background server, no runtime, no version skew. One 11 MB static-ish binary. |
| **Many agents at once** | 2, 4, 6, 8 sessions tiled side by side. Plan in one pane while another builds and a third repairs tests. |
| **Any model, per pane** | Live model discovery across every provider you've logged into. Pane 1 on a cheap fast model, pane 2 on a frontier model, pane 3 on local weights. |
| **History you can rewind** | Sessions are a *tree*, not a log. `/undo` restores the files a turn touched, `/redo` puts it back, `/fork` branches without disturbing the original. |
| **Costs you can see** | Context size and spend sit in every pane footer, and every request is traceable in `/logs`. |

## Install

**Linux and macOS**

```sh
curl -fsSL https://raw.githubusercontent.com/Bhanu4417/theta/main/install.sh | sh
```

**Windows** (PowerShell)

```powershell
irm https://raw.githubusercontent.com/Bhanu4417/theta/main/install.ps1 | iex
```

Both download the release for your platform, **verify its SHA-256**, and install
the binary. Nothing else is touched. Set `THETA_INSTALL_DIR` to choose where it
goes (`~/.local/bin` on Linux/macOS, `%LOCALAPPDATA%\Programs\Theta` on
Windows), or pass `-Version`/`--version` to pin a release.

<details>
<summary>Other ways to install</summary>

**Prebuilt binary.** Grab the archive for your platform from
[Releases](https://github.com/Bhanu4417/theta/releases), check it against
`SHA256SUMS`, and put the binary on your `PATH`.

**cargo-binstall** — same binary, no compile:

```sh
cargo binstall theta
```

**Homebrew** — once the tap is published (see `packaging/homebrew/` for the
one-time setup):

```sh
brew tap Bhanu4417/tap
brew install theta
```

**From source.** Stable Rust **1.88+** is required (a dependency floor, not a
choice):

```sh
git clone https://github.com/Bhanu4417/theta
cd theta
cargo build --release
install -Dm755 target/release/theta ~/.local/bin/theta
```

**Cargo:**

```sh
cargo install --path .
```

</details>

**Platforms.** Linux (x64, arm64), macOS (Apple Silicon, Intel) and Windows
(x64) all have prebuilt binaries, and CI builds and tests on all three. One
Windows caveat: the `bash` tool and the `!cmd` shell escape need a `bash` on
your `PATH`, so use Git for Windows or WSL.

**Uninstall.** Delete the binary. Your sessions, keys and config live in
`~/.config/theta` and `~/.local/share/theta` and are left alone.

Then:

```sh
theta                    # restore your last workspace
theta ~/projects/app     # open rooted at a directory
theta --no-restore       # start clean
```

## First run

```sh
theta
/login          # pick a provider, paste an API key
/model          # browse every model your keys can reach
```

Keys are stored in `~/.config/theta/keys.toml` with `0600` permissions.
Or skip the prompt entirely:

```sh
THETA_AI_PROVIDER=anthropic THETA_AI_API_KEY=sk-… theta
theta --check-ai                       # verify every configured provider
theta --print "summarize this repo"    # one headless turn
```

## Keys

| Key | Action |
| --- | --- |
| `^N` / `^R` | New session · resume a previous one |
| `^W` / `^Q` | Close session · quit |
| `^K` | Command palette |
| `^T` / `^Space` | Change tiling · maximize the focused pane |
| `Tab` / `S-Tab` | Cycle panes |
| `Alt+←↑→↓` | Focus the pane in that direction |
| `Alt+Shift+←↑→↓` | Move (swap) a pane |
| `Ctrl+Alt+←↑→↓` | Resize a pane |
| `Alt+1..9` | Focus the *n*th pane |
| `^C` / `Esc Esc` | Interrupt the focused agent |
| `^P` / `^⇧F` / `^F` | Search files · project contents · conversation |
| `^B` | Toggle the file explorer |
| `@` | Mention a file (fuzzy picker, attaches it) |
| `!cmd` / `!!cmd` | Run a shell command, with / without sending output to the agent |
| `^G` | Compose the prompt in `$EDITOR` |
| `^V` | Paste text or a clipboard image |
| `↑` | Recall the previous prompt |
| `PgUp` / `PgDn` | Scroll the transcript (`End` re-follows) |
| `↓` / `Enter` | Select a tool entry · expand–collapse it |
| `d` / `o` | Diff · open the selected tool's file |
| `a` / `A` / `r` | Permission: allow once · always · reject |
| `^Y` | Copy the last reply or selected tool output |
| `F1` | Help |

Every binding is remappable — `/keys` edits them live.

## Commands

Type `/` in any session for the menu (filter by typing, `↑↓` to choose).

| | |
| --- | --- |
| `/model` `/agent` | Switch model or agent for this session |
| `/new` `/resume` `/sessions` `/close` `/rename` `/delete` | Session lifecycle |
| `/compact` | Fold the conversation into a summary on demand |
| `/undo` `/redo` | Rewind with per-turn diff stats, restoring files |
| `/tree` `/fork` | Jump to any earlier point · branch a session into a new pane |
| `/clear` `/export` | Clear the view (history kept) · export Markdown or JSONL |
| `/logs` | Tail the request log live, in-TUI |
| `/editor` | Compose the prompt in `$EDITOR` |
| `/push` | Commit and push the session's project |
| `/refresh` | Re-exec the newest build without leaving |
| `/reasoning` | How hard the model thinks: `none`…`max`. Lower is faster and
  avoids an empty reply when reasoning eats the answer budget |
| `/login` `/help` `/quit` | Providers · keys · exit |

## When a reply comes back empty

A reasoning model can spend its whole output budget thinking and emit no answer.
Theta retries that once with minimal thinking; if it is still empty you get a
note telling you what to try, rather than a reply that is simply missing.

`/reasoning` changes how hard the model thinks, for every following turn:

```
/reasoning minimal      # fastest; least likely to run out of room
/reasoning low          # a good default for a busy gateway
/reasoning default      # hand the choice back to the provider
```

`none` disables thinking entirely on providers that allow it. The setting lives
in `[ai] reasoning_effort`, and `/reasoning` changes it without editing a file
or restarting.

## Agents

Four built-in agents, each with its own tool set, prompt and permissions.

| Agent | Tools | Use it for |
| --- | --- | --- |
| `build` | everything | Implementing, end to end |
| `plan` | read-only | Understanding before changing |
| `general` | everything | A delegated sub-task |
| `explore` | read-only | Searching and reporting |

`plan` and `explore` **cannot write files** — enforced by the permission gate,
not by prompt wording. `task` lets an agent delegate a focused sub-task to a
nested sub-agent with no interactive prompts.

## Session history is a tree

Every turn is a node with a parent. Compaction and branch summaries are
first-class nodes that rebuild the model context.

```
● system ─ ○ you ─ ● ai ─ ○ you ─ ● ai ─ ⬢ compaction ─ ● ai     ← active branch
                            └○ you ─ ● ai ─ ○ you                  ← still on disk
```

- **`/undo`** — pick a turn, see its `+adds −dels` stats, and rewind. The files
  that turn changed are restored from recorded pre-images, the prompt returns to
  the chatbox, and the abandoned branch stays on disk.
- **`/redo`** — put it back.
- **`/fork`** — duplicate a session (optionally pinned to an earlier point).
- **`/tree`** — jump anywhere; the abandoned work is summarized and injected so
  the new branch remembers what you tried.

Sessions persist to `~/.local/share/theta/sessions/<id>.jsonl`.

### When the provider is having a bad day

A busy model behind a shared gateway returns `500`s and `429`s for minutes at a
time. Theta retries those rather than surfacing an error:

- **What is retried:** `408`, `425`, `429`, and every `5xx` — plus dropped
  connections and timeouts.
- **How long:** `Retry-After` is honoured when the server sends one (either
  seconds or an HTTP date, both forms). Otherwise exponential backoff from
  `retry_base_ms`, capped at `retry_max_ms`.
- **How many:** unlimited by default (`max_retries = 0`). A busy gateway can be
  unavailable for minutes, and giving up mid-turn loses the work. Set
  `max_retries` to a number to bound it.
- **How it looks:** the pane title counts down and shows the reason —
  `↻ 7s (attempt 4) · provider error (500)` — so a slow model reads as
  *waiting*, not frozen. **Ctrl+C stops it at any point**, including during the
  backoff itself.
- **What is not retried:** `401`/`403` (a key problem) and `404` (a wrong
  endpoint). Retrying those just wastes time.

### Why it stays responsive

Latency work is not one thing, so it is attacked at each layer:

- **Reasoning effort is sent on every provider.** Left unset, a reasoning model
  decides for itself how long to think, and models default high. A configured
  `reasoning_effort` is translated per provider — `reasoning_effort` on
  OpenAI-compatible endpoints, a thinking budget on Anthropic, a
  `thinkingBudget` on Gemini (`minimal` disables thinking outright).
- **Only what changed is re-rendered.** Each message's rendered lines are cached
  and reused, so streaming a reply costs one message of work rather than the
  whole transcript. On a 509-message session that is ~100x less per token. The
  cache key includes whether the turn is running and which message is live, so a
  frame rendered mid-turn is never reused once it is stale.
- **Independent tool calls run concurrently.** A turn that reads three files or
  runs two searches no longer waits on each in series. Mutating tools stay
  sequential so they cannot race.
- **Connections are pooled and un-buffered.** TCP_NODELAY, HTTP/2 and aggressive
  connection reuse, so a turn does not pay a fresh TLS handshake or wait on
  Nagle's algorithm for each streamed frame.
- **Anthropic prompts are cached.** The system prompt and tool schemas carry
  cache breakpoints, turning a full prefill into a cache read.

### Keeping long sessions affordable

Every request re-sends the conversation, so anything left in context is paid
for on **every following turn**. Tool output is the usual culprit — a single
`cargo build` or `webfetch` can be tens of thousands of tokens.

Theta keeps a **rolling window of recent tool output** (40k tokens by default).
Older output is replaced by a one-line stand-in before the request goes out, so
the model is never charged for it again. The current turn and the one before it
are always left intact, and the message itself stays in place — only its bulk is
elided — so the conversation remains valid for strict providers.

This costs nothing: there is no model call involved, and it usually means a
session never needs summarizing at all. When the prompt genuinely outgrows the
window, Theta summarizes; if a provider still rejects a request as too large,
Theta compacts and retries rather than failing the turn.

The trade-off is honest: **old tool output is gone from the model's context.**
If it needed a value from a command it ran twenty turns ago, it will re-run it.
Your transcript on disk keeps everything. Tune it with
`[compaction] prune`, `prune_protect_tokens` and `prune_minimum_tokens`.

## Configuration

`~/.config/theta/config.toml` is written with defaults on first run.

```toml
[ai]
provider = "openai"             # a preset id, or set base_url for anything else
base_url = ""                   # an OpenAI-compatible endpoint overrides the preset
api_key_env = "OPENAI_API_KEY"
model = "gpt-4o"
max_retries = 0                 # 0 = keep retrying; N = give up after N
retry_base_ms = 500             # first backoff; doubles each attempt
retry_max_ms = 30000            # backoff ceiling
timeout_secs = 300              # idle read timeout; 0 disables
max_turns = 0                   # tool rounds per turn; 0 = no limit
reasoning_effort = ""           # none | minimal | low | medium | high | xhigh | max

[compaction]
enabled = true
reserve_tokens = 16384          # headroom left for the reply
keep_recent_tokens = 0          # 0 = adapt to the model's window
prune = true                    # roll old tool output out of the prompt
prune_protect_tokens = 40000    # recent tool output kept verbatim
prune_minimum_tokens = 20000    # only prune once this much would be freed
model = ""                      # optional cheap model for summaries

[ui]
restore = true                  # reopen your last workspace
explorer_width = 32
history_limit = 200

[behavior]
auto_approve_permissions = false
confirm_quit = true
notify = true                   # bell + desktop notification when a run ends
local_permissions = "allow"     # allow | ask | deny | read-only
```

Per-model overrides, when one model needs different budgets:

```toml
[compaction.model_overrides."openai/gpt-4o"]
reserve_tokens = 200000
```

MCP servers are declared and their tools become available to the agent:

```toml
[mcp.files]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/some/dir"]
```

Provider presets: `anthropic`, `google`, `openai`, `xai`, `groq`, `deepseek`,
`openrouter`, `together`, `fireworks`, `mistral`, `cerebras`, `perplexity`,
`ollama` — plus any OpenAI-compatible gateway via `base_url`, and the hosted
gateways whose ids appear in `/login`.

## Headless mode

Theta's harness is scriptable.

```sh
theta --print "list every TODO in src/"      # one turn, print the reply
theta --print --json "count the tests"       # stream neutral events as JSONL
```

Skills and prompt packs are discovered from `~/.config/theta/skills/`,
`./.theta/skills/`, `.agents/skills/` and `…/prompts/<name>.md`, and indexed
into the agent's system prompt.

## Architecture

```
src/
├── main.rs            terminal setup, event loop, re-exec on /refresh
├── app.rs             state, key routing, overlays, commands
├── events.rs          AppEvent — the single channel from harness to UI
├── harness/           provider-neutral events + transcript model
├── providers/         AgentProvider + EventPump contract, local adapter
├── ai/                Provider trait: OpenAI, Anthropic, Gemini, Responses
├── agent/             agent loop, tools, named agents, compaction
├── mcp.rs             stdio MCP client, dynamic mcp__ tools
├── tree.rs            session history tree
├── panes.rs           weighted tiling grid
├── ui/                header, panes, transcript, overlays, status bar
└── …
```

The UI never talks to a provider. Background work reports back as `AppEvent`s,
and rendering is event-driven from a per-session line cache that rebuilds only
when something actually changed.

A few invariants are load-bearing:

- Harness calls are fire-and-forget; the event loop is never blocked on I/O.
- The provider mutex is never held across an `await`.
- Anything long-running is capped or run off the render path.
- Hooks restore the terminal on panic, so a crash never leaves a broken shell.

## Testing

```sh
cargo test      # 189 tests, no network required
cargo clippy    # clean
```

The suite is deterministic and offline: scripted providers drive the agent
loop, and the tree, compaction, permission and rendering paths are all covered
by unit tests next to the code they test.

## Contributing

Issues and PRs are welcome. `AGENTS.md` documents the architecture and the
conventions this codebase holds to; `cargo test` must pass before a change is
considered done.

## License

MIT — see [LICENSE](LICENSE).

<div align="center">
<br>
<sub>Θ — for people who keep four things going at once.</sub>
</div>
