# Θ Theta

**A terminal-native multi-agent workspace with its own in-process agent harness.**

Run multiple agent sessions simultaneously in one terminal and view them as
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

Requires the stable Rust toolchain (edition 2021).

Install it so you can just type `theta` anywhere:

```sh
cargo build --release && install -Dm755 target/release/theta ~/.local/bin/theta
```

## Running

```sh
theta                  # restore last workspace (or show empty state)
theta ~/projects/app   # open with the workspace rooted at DIR
theta --no-restore     # skip workspace restore
theta --log            # open/tail the newest debug log
theta --log --run      # start the TUI, recording a verbose debug log
```

`theta --log --run` records a timestamped trace to
`~/.local/share/theta/logs/theta-<epoch>.log` (absolute path, so it works from
any directory) covering CLI args, provider **requests → provider/model** and
responses (latency, tokens), model/agent switches, permissions, and panics.
`theta --log` (no `--run`) opens the newest log in `less +F` (falls back to
`tail -f`). While a logging TUI is running, `/logs` tails the same file in place
(`↑/↓` scroll, `f` follow, `Esc` close) so you can watch exactly which model
each request hits without a second terminal:

```
[05:47:08.109] REQ  provider=openai-compat model=deepseek-v4.1-flash msgs=2 tools=10
[05:47:10.275] RESP model=deepseek-v4.1-flash ms=2166 chars=4 tool_calls=0 tokens=5269/3
```

Theta **runs its own agent harness in-process** — no external agent binary. It
streams model output, executes tools, gates them by permission, compacts
context, and persists every turn to a history tree.

## Slash commands

Type `/` in any session input for the command menu (filter by typing,
`↑/↓` to choose, `Enter` to run, `Tab` to complete):

| Command | Action |
| --- | --- |
| `/model [provider/model]` | Live-fetch models from every logged-in provider and switch (searchable picker) |
| `/logs` | Live-tail the debug log in the TUI (requests → provider/model) |
| `/agent [name]` | Switch agent (build, plan, …) |
| `/new` | New session |
| `/sessions` | Jump to another open session |
| `/clear` | Clear the transcript view (history kept) |
| `/compact` | Summarize the conversation |
| `/undo` / `/redo` | Revert / re-apply the last message |
| `/share` / `/unshare` | Share the session (no share links; suggests `/export`) |
| `/init [focus]` | AGENTS.md setup — currently reports it is unsupported |
| `/refresh` | Reload the newest build in place (soft restart) |
| `/export [file]` | Export this session (Markdown, or JSONL if `.jsonl`) |
| `/editor` | Compose the prompt in `$EDITOR` (also `Ctrl+G`) |
| `/tree` | Jump to an earlier point in a local session's history tree |
| `/push [message]` | Commit all changes and push the session's project |
| `/rename [name]` | Rename this session |
| `/delete` | Delete session from the workspace (history kept) |
| `/help` · `/quit` | Keys · quit |

The chosen model and agent are used for subsequent prompts in that session and
shown in the status bar.

`/refresh` saves the workspace and re-executes the `theta` binary so a freshly
built version takes over without quitting and reopening by hand. Session
history is restored from the local session tree when the new process starts.
Questions surface as a picker — navigate with `↑/↓`, toggle with `Space`
(multi-select), confirm with `Enter`, reject with `Esc`.

`/push` is available only in a session's input box (not the global command
palette). It stages everything in that session's directory, commits with the
given message (or a concise auto-generated subject when omitted), and pushes —
with the commit status and a small monochrome "git push" animation shown in
the status bar, then `Git pushed "commit subject" owner/repo`.

## Keys

| Key | Action |
| --- | --- |
| `^N` | New session (name + directory, or resume a recent one) |
| `^R` | Resume a previous session (searchable list) |
| `^W` | Close session |
| `^K` | Command palette |
| `^T` | Change tiling (auto grid / rows / columns) |
| `^Space` | Maximize / restore pane |
| `Tab` / `S-Tab` | Cycle pane focus |
| `Alt+← ↑ → ↓` | Focus the pane in that direction |
| `Alt+Shift+← ↑ → ↓` | Move (swap) the pane in that direction |
| `Alt+Ctrl+← ↑ → ↓` | Resize the pane in that direction |
| `Alt+h j k l` | Resize pane left / down / up / right (alias) |
| `Alt+Shift+h j k l` | Move pane in a direction (alias) |
| `Alt+1..9` | Focus nth pane |
| `^P` | Search files (local, gitignore-aware) |
| `^⇧F` (or `Alt+⇧F`) | Search project content |
| `^F` | Search current conversation |
| `^B` | Toggle file explorer panel |
| `^C` | Interrupt the focused agent |
| `Esc` `Esc` | Interrupt the focused agent (double-tap; hint by the cost) |
| `Shift+Enter` | Newline in the prompt (also `Alt+Enter` / `Ctrl+J`) |
| `@` | Mention a file: fuzzy picker, attaches it to the prompt |
| `!cmd` / `!!cmd` | Run a shell command (send / don't send output to the agent) |
| `Ctrl+G` | Edit the prompt in `$EDITOR` |
| `Ctrl+V` | Paste text or a clipboard image (long pastes collapse to `[Pasted ~N lines]`) |
| `↑` | Recall the previous prompt; select a tool entry when one is active |
| `PgUp` / `PgDn` / wheel | Scroll transcript (`End` re-follows) |
| `↓`, `Enter` | Select a tool entry / expand–collapse it |
| drag mouse | Select transcript text and copy it (OSC 52) |
| `Ctrl+Y` | Copy the last reply / selected tool output |
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
including full transcript history replayed from the local session tree. The
new-session dialog skips model picking (the last model you picked, or the
configured default, is used; change it any time with `/model`) and lists
recent local sessions to resume.

## Usage metrics

Like the OpenCode TUI, Theta surfaces live usage: per-session prompt context
size (`ctx 4%` when the model's context limit is known, raw tokens otherwise)
and cost in the pane footer, immediately left of the folder path —
`ctx 4% · $0.0012 · ~/projects/my-app`. Thinking shows an animated elapsed
timer while the model reasons and a quiet `✓ thought for 4.7s` marker
afterwards; the reasoning text itself is not previewed (streaming it made the
transcript flicker), but it stays searchable with `Ctrl+F`.

## Configuration

`~/.config/theta/config.toml` (written with defaults on first run):

```toml
# Theta's own harness: LLM provider + model.
[ai]
provider = "openai"      # opencode | opencode-go | anthropic | google | openai |
                         # xai | groq | deepseek | openrouter | together |
                         # fireworks | mistral | cerebras | perplexity | ollama
base_url = ""            # set for a custom/compatible endpoint (overrides provider)
api_key_env = "OPENAI_API_KEY"   # fallback when no key is stored
model = "gpt-4o"         # opencode → glm-4.7, opencode-go → deepseek-v4.1-flash,
                         # anthropic → claude-3-7-sonnet-20250219,
                         # google → gemini-2.0-flash, xai → grok-2-latest, …

# Context compaction (local backend), Pi-style token budgets.
[compaction]
enabled = true
reserve_tokens = 16384   # headroom left for the model's response
keep_recent_tokens = 20000  # recent tokens kept verbatim, rest summarized
model = ""               # optional fast/cheap model for summaries ("" = session model)
# Summary output is hard-capped at 4k tokens so /compact stays quick on any model.

# Optional per-model overrides (keyed provider/model or bare model).
[compaction.model_overrides."openai/gpt-4o"]
reserve_tokens = 400000

[ui]
restore = true           # restore last workspace on startup
explorer_width = 32

[behavior]
auto_approve_permissions = false
confirm_quit = true
notify = true            # bell + desktop notification when a run finishes
local_permissions = "allow"  # allow (default, OpenCode-style: no prompts) |
                             # ask | deny | read-only. In "ask", reads inside
                             # the project are auto-allowed; only edits,
                             # commands and out-of-tree reads prompt.
```

Theta runs its own harness — there is no external agent binary to install or
wrap. Run `/login` to open a provider picker, choose a provider and type its
API key; keys are saved to `~/.config/theta/keys.toml` (chmod `0600`). The
list mirrors the OpenCode CLI: **OpenCode Zen** (`opencode`, `OPENCODE_API_KEY`)
and **OpenCode Go** (`opencode-go`) come first, then Anthropic, OpenAI, Google,
xAI, Groq, DeepSeek, OpenRouter, Together, Fireworks, Mistral, Cerebras,
Perplexity and local Ollama. Choosing one makes it the active provider and
applies its default model immediately. `/model` then queries every provider you
have logged into (`GET /models`, or the native Anthropic/Gemini list endpoints)
and shows the full live catalog; the built-in list is the fallback. Fetched
lists are cached in `~/.cache/theta/models.json`, so later `/model` opens do not
re-fetch; logging in again with a new key refreshes just that provider. Zen/Go
models served only through the OpenAI **Responses API** (`muse-spark-*`,
`gpt-5*`, `grok-4*`) are routed to `/responses` automatically. Pick
**custom (OpenAI-compatible URL)**
to point Theta at any gateway: enter the base URL, then the key, and Theta
stores it as the active endpoint. The matching `ai.api_key_env` variable is
used as a fallback. Legacy `/login <provider> <key>` still works
non-interactively (and activates known providers). `THETA_AI_PROVIDER`,
`THETA_AI_MODEL`, `THETA_AI_BASE_URL` and `THETA_AI_API_KEY` override `[ai]`
for a single run (handy for testing). `theta --check-ai [provider…]` sends a
tiny live request per provider and reports OK/FAIL.

Provider round-trips are unlimited by default (`ai.max_turns`, `0` = no cap):
the model runs until it finishes and `Ctrl+C` interrupts. Set a positive number
for an automatic safety stop. Transient provider failures are retried with
exponential backoff (`ai.max_retries`, `ai.retry_base_ms`), and every request is
bounded by
`ai.timeout_secs` (default 300, `0` disables) so a stalled gateway can never
hang a turn or `/compact`. `ai.reasoning_effort` (`minimal`/`low`/`medium`/
`high`) tunes reasoning models such as `muse-spark`/`gpt-5`/`grok-4`; `high`
(the provider default) can spend a whole small budget thinking and return no
text, so compaction always summarizes at `low`. **Named agents** (`build`, `plan`,
`general`, `explore`) select their own tool set and prompt — `plan`/`explore`
are read-only — and `plan`/`explore`/`general` are available as `task`
sub-agent types. **MCP** stdio servers can be declared and their tools are
exposed to the agent:

```toml
[mcp.files]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/some/dir"]

[mcp.git]
command = "uvx"
args = ["mcp-server-git"]
```

## Harness, headless mode & skills

```sh
theta --print "summarize this repo"       # one local turn, print the reply
theta --print --json "list the files"     # stream neutral events as JSON lines
```

Theta runs its **own agent loop**: a provider plus built-in tools (`read`, `write`, `edit`,
`multiedit`, `bash`, `grep`, `glob`, `webfetch`, `ask`, `task`), automatic
context compaction, and the same event stream the UI renders. Native
**Anthropic** (Messages API) and **Google Gemini** providers are built in.
Switching models in the UI rebuilds the local provider on demand, `@file` and
pasted images are inlined into the prompt, `ask` questions and tool permissions
surface as interactive prompts (`a`/`A`/`r`; `behavior.local_permissions`), and
`task` delegates a focused sub-task to a nested, auto-approved sub-agent. Any OpenAI-compatible gateway works — set
`ai.provider` to a preset or point `ai.base_url` at a custom endpoint, so
OpenAI, xAI/Grok, Groq, OpenRouter, DeepSeek, Together, Fireworks, Ollama and
LM Studio are all supported without code changes.

**Session history is a tree.** Every local turn is stored as linked entries
(`~/.local/share/theta/sessions/<id>.jsonl`, override with `THETA_SESSION_DIR`).
`/tree` opens a navigator; jumping to an earlier entry summarizes the
abandoned branch with the model, injects that summary, and fans out a new
branch while the old one is kept on disk. `/resume` lists saved local sessions,
and `/compact` folds the current context into a summary on demand.

**`/undo` rewinds like the agy CLI**: a picker pops up above the chatbox listing
your prompts with per-turn diff stats (`+adds -dels`). Choosing one moves the
history leaf before it (the branch is kept on disk), restores the files that
turn changed from recorded pre-images, drops the turn's output from the view,
and puts the prompt back in the chatbox; `/redo` restores it. `/fork` duplicates
a session (optionally pinned to an earlier point) without touching the source. Compaction and branch summaries are
first-class nodes that rebuild the model context (`id`/`parentId`, like Pi).

**Skills and prompt packs** are discovered from `~/.config/theta/skills/`,
`./.theta/skills/`, `.agents/skills/` (SKILL.md or `<name>.md`) and
`…/prompts/<name>.md`; skills are indexed into the local agent's system prompt.

## Architecture

```text
src/
├── main.rs            terminal setup + event loop; --print/--json headless
├── app.rs             state, key routing, commands, overlay state
├── events.rs          AppEvent: manager → UI channel
├── harness/           provider-neutral HarnessEvent + transcript model
├── providers/         AgentProvider + EventPump contract + local adapter
├── ai/                LLM layer: Provider trait, OpenAI/Anthropic/Gemini, catalog
├── agent/             agent loop + tools + named agents + context compaction
├── mcp.rs             stdio MCP client + dynamic mcp__ tools
├── extensions.rs      skills / prompt-pack registry
├── panes.rs           weighted row/column grid: auto-tile, resize, swap
├── session.rs         per-session transcript, input, status, adoption
├── manager.rs         local harness bridge: async work → AppEvent, pump
├── models.rs          shared session/model/question value types
├── git.rs             async git CLI (branch/status/log/diff), cached
├── fsx.rs             gitignore-aware listing + local search fallback
├── highlight.rs       syntect with an embedded Tokyo Night theme
├── config.rs          ~/.config/theta/config.toml
├── persist.rs         workspace save/restore
└── ui/                header, panes, conversation cache, overlays
```

Rendering is event-driven: keyboard/harness events mutate state, the draw
pass renders from a per-session line cache that rebuilds only when the
transcript, width, or spinner tick changes.
