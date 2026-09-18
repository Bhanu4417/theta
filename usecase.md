# What to use, and when

A practical guide to Theta. Organised by *what you are trying to do*, not by
command name, because the command name is the thing you don't know yet.

Every command here was checked against the code, so if it's listed, it works.
Two commands are deliberately listed as non-functional (`/share`, `/init`) rather
than quietly omitted.

---

## The only five things you need on day one

If you read nothing else, read this.

| When | Do this |
|---|---|
| Open a project | `theta ~/code/my-app` in a terminal |
| Ask for work | Type it in the bottom box and press `Enter` |
| Give it a different job | `/agent plan` first for a plan, `/agent build` to implement |
| It wants to run something risky | Press `a` to allow once, `A` always, `r` to refuse |
| Get out | `Ctrl+Q` |

That is a complete workflow. Everything below is for going faster or handling
fancier situations.

---

## Starting out

```sh
theta                       # reopen your last workspace, exactly as you left it
theta ~/code/my-app         # start with a session in that project
theta --no-restore          # start clean, ignore the last workspace
```

First run, you need credentials:

```sh
/login                      # inside Theta: pick a provider, paste an API key
```

Or skip the prompt by setting an environment variable:

```sh
ANTHROPIC_API_KEY=sk-... theta
```

To check all your credentials are alive without opening anything:

```sh
theta --check-ai            # tests the configured provider
theta --check-ai anthropic google    # tests specific ones
```

---

## By task

### "I want to understand a codebase I just opened"

```
/agent plan
Explain how authentication flows through this project.
```

`plan` is **read-only**. It cannot edit a file even if it wanted to, because the
permission gate blocks writes — not because the prompt asks it nicely. Use it
whenever you're unsure and don't want anything touched.

### "I want this feature built"

```
/agent build
Add rate limiting to the /api/login endpoint, and write a test.
```

`build` has every tool. This is the default.

### "This is a big job — I want it planned first, then done"

The single most useful habit in Theta: **use two panes.**

```
Left pane:   /agent plan     Investigate and write an ordered plan.
Right pane:  /agent build    Implement it and run the tests.
```

`Ctrl+N` makes a new pane. `Tab` moves between them. You watch both work at once
instead of waiting for one to finish.

### "It's taking ages and I want to know why"

```
/logs
```

Shows every request live: which provider, which model, how long, how many tokens,
and whether it retried. If a turn is slow, this tells you whether the model is
slow, the network is slow, or it's retrying.

Also visible without any command: the pane footer shows
`build · provider/model    ctx 12% · $0.04 · ~/code/app` — the model in use, how
full the context is, and what you've spent.

### "It keeps replying badly / too slowly"

```
/reasoning minimal      # fastest, least thinking
/reasoning low          # a good default for a busy gateway
/reasoning high         # hardest thinking, slowest
/reasoning default      # let the provider decide
```

Reasoning models spend time "thinking" before answering. Lower effort is faster
and cheaper. This applies to the next message immediately — no restart.

If you get an empty reply, Theta already retries once with minimal thinking. If
it's *still* empty, the model is the problem, not you.

### "I want a different model"

```
/model
```

Lists every model from every provider you're logged into. Pick one, it applies
to that pane. Because it's per-pane, one pane can run a cheap fast model while
another runs a frontier one.

### "I changed my mind — undo that"

```
/undo
```

Shows your turns with `+adds −dels` for each. Pick one and Theta:

- moves the conversation back to that point,
- **restores the files that turn changed**,
- puts your prompt back in the box so you can edit and resend.

The abandoned branch is not deleted, so nothing is lost. `/redo` puts it back.
`/tree` jumps anywhere and summarises what you abandoned, so the model still
knows what you tried.

This is the feature that makes Theta worth using over a plain chat: you can
experiment fearlessly because going back is cheap.

### "I want to try this idea without wrecking my current session"

```
/fork
```

Clones the session into a new pane, leaving the original untouched. Now you can
run an experiment in one and keep working in the other.

### "The conversation is getting long and expensive"

```
/compact
```

Folds the conversation into a summary. Usually you don't need to — Theta prunes
old tool output automatically, and the footer's `ctx %` shows where you stand.
Use this when `ctx` is high and you want to keep going in the same session.

Cheaper alternative: start a fresh session for a new topic. A long session
re-sends its whole history on every message.

### "I want to run a shell command"

Typed straight into the prompt box:

```
!git status              # runs it, and sends the output to the agent
!!git status             # runs it, output stays out of the conversation
```

Use `!!` when the output is noise you don't want billed on every future message.

### "I want the agent to look at specific files"

```
@src/auth.rs what does this do?
```

`@` attaches the file. You can mention several. It also accepts directories.

### "My prompt is long — I want a real editor"

`Ctrl+G` opens `$EDITOR`. Write comfortably, save and quit, and it becomes your
prompt. Same as `/editor`.

### "I want to see what it changed"

Every edit shows the lines it changed, in diff form:

```
 ✓ Editing src/app.rs
     @@ -12,3 +12,3 @@
      let cfg = Config::default();
     -    let retries = 3;
     +    let retries = 6;
```

- `Enter` on a tool entry — expand to the full change
- `d` on a tool entry — open the diff (side by side when the window is wide)
- `o` — open the file it touched

Diffs are saved with the session, so they're still there after a restart.

### "I want to copy what it said"

`Ctrl+Y` copies the last reply, or the selected tool output. There's also
`/export` for Markdown or JSONL of the whole session.

### "I want to jump between sessions I already have open"

`Ctrl+R` lists previous sessions to resume, `Ctrl+N` makes a new one, and
`/sessions` jumps between the ones open right now.

`/rename` names the current session — useful when you have four panes and can't
tell which is which. The name sticks across restarts.

### "I want more than one agent working at once"

```
Ctrl+N        new pane (a second agent, its own folder, model and agent)
Tab           move between panes
Ctrl+Space    maximise the focused pane
Alt+arrow     move focus in that direction
Alt+Shift+arrow  move (swap) a pane
Ctrl+Alt+arrow   resize a pane
Ctrl+T        change the tiling arrangement
Ctrl+W        close a pane
```

A common setup: pane 1 planning, pane 2 building, pane 3 running tests, pane 4
on a different repo entirely.

### "I want to search"

| Key | Searches |
|---|---|
| `Ctrl+P` | file names |
| `Ctrl+Shift+F` | file contents (the project) |
| `Ctrl+F` | the conversation |
| `Ctrl+B` | toggle the file explorer |

### "I want to save my work"

```
/push
```

Commits and pushes the project for that session. The workspace status bar shows
the branch and what's outstanding: `main ↑2 ↓1 +48 -12 ?3` — ahead 2, behind 1,
48 lines added, 12 removed, 3 untracked.

### "I want to connect an external tool"

MCP servers are declared in `~/.config/theta/config.toml`:

```toml
[mcp.files]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/some/dir"]
```

Their tools become available to the agent, named `mcp__<server>__<tool>`.

---

## Do this, not that

Real advice, learned from using it.

| Instead of | Do | Why |
|---|---|---|
| One long session for everything | A new session per topic | Every message re-sends the whole history |
| `!cargo build` (huge output) | `!!cargo build` | The warning dump is billed on every later message |
| `/agent build` on unfamiliar code | `/agent plan` first | Read-only, so it can't break anything |
| Watching one pane work | Two panes, plan + build | You're the bottleneck otherwise |
| `/compact` constantly | Watch `ctx %`, compact when high | Theta prunes tool output automatically |
| Accepting every permission | `a` once, `A` only for safe tools | `A` is permanent for that tool |
| Asking "why is it slow" of the model | `/logs` | It tells you whether it's the model, the net, or retries |

---

## Every command

Type `/` for the menu, or the whole word.

| Command | What it does |
|---|---|
| `/model` | Change the model for this pane |
| `/reasoning` | How hard it thinks: `none minimal low medium high xhigh max default` |
| `/agent` | Switch agent: `build`, `plan`, `general`, `explore` |
| `/new` | New session |
| `/sessions` | Jump to another open session |
| `/resume` | Resume a previous session from disk |
| `/rename` | Name this session |
| `/close` | Close this session |
| `/delete` | Remove it from the workspace (history kept on disk) |
| `/fork` | Clone this session into a new pane |
| `/undo` | Rewind, restoring the files that turn changed |
| `/redo` | Re-apply the last rewind |
| `/tree` | Jump to any earlier point, summarising what you abandoned |
| `/compact` | Summarise the conversation now |
| `/clear` | Clear the view (history kept — nothing is lost) |
| `/export` | Export as Markdown or JSONL |
| `/logs` | Live request log: provider, model, latency, tokens, retries |
| `/push` | Commit and push this project |
| `/editor` | Compose the prompt in `$EDITOR` (also `Ctrl+G`) |
| `/keys` | View and remap keybindings |
| `/login` | Add a provider API key |
| `/refresh` | Reload the newest Theta build without quitting |
| `/help` | Keys and commands overview |
| `/quit` | Quit |
| `/share` | **Does nothing.** There are no share links — use `/export` |
| `/init` | **Does nothing.** Skills load automatically; edit `AGENTS.md` yourself |
| `/unshare` | Only exists to undo `/share`, which does nothing |

---

## Every key

| Key | Action |
|---|---|
| `Enter` | Send. While busy: queue it, or a menu appears |
| `Ctrl+N` / `Ctrl+R` | New session / resume an old one |
| `Ctrl+K` | Command palette |
| `Ctrl+W` / `Ctrl+Q` | Close pane / quit |
| `Ctrl+C` | Interrupt the agent — works during retries too |
| `Ctrl+Space` | Maximise the focused pane |
| `Ctrl+T` | Change tiling |
| `Tab` / `Shift+Tab` | Next / previous pane |
| `Alt+arrows` | Focus the pane in that direction |
| `Alt+Shift+arrows` | Move (swap) a pane |
| `Ctrl+Alt+arrows` | Resize a pane |
| `Ctrl+P` / `Ctrl+Shift+F` / `Ctrl+F` | Search files / project / conversation |
| `Ctrl+B` | Toggle the file explorer |
| `Ctrl+G` | Compose the prompt in `$EDITOR` |
| `Ctrl+Y` | Copy the last reply or selected tool output |
| `PageUp` / `PageDown` | Scroll the transcript (`End` re-follows) |
| `Up` / `Down` | Select a tool entry |
| `Enter` on a tool entry | Expand / collapse it |
| `d` / `o` | Diff / open the selected tool's file |
| `a` / `A` / `r` | Permission: allow once / always / reject |
| `F1` | Help |

**In permission prompts** (the agent wants to run something): `a` allows once,
`A` allows that tool forever, `r` or `Esc` refuses.

**In a question prompt** (the agent asks you something with `ask`): arrow keys to
choose, `Enter` to confirm.

---

## What the agent can actually do

Ten tools. This is the boundary of its power.

| Tool | Purpose |
|---|---|
| `read` | Read a file |
| `write` | Create or overwrite a file |
| `edit` | Replace one exact piece of text in a file |
| `multiedit` | Several edits at once, all or nothing |
| `bash` | Run a shell command |
| `grep` | Search file contents |
| `glob` | Find files by pattern |
| `webfetch` | Fetch a URL |
| `task` | Delegate a sub-task to a sub-agent |
| `ask` | Ask you a question mid-task |

Plus any MCP tools you configure, and `mcp__<server>__<tool>` naming.

---

## Which agent to use

| Agent | Tools | Use when |
|---|---|---|
| `build` | everything | Implementing. The default |
| `plan` | read-only | You want understanding before changes, and a guarantee nothing is touched |
| `explore` | read-only | Searching and reporting — as a sub-agent |
| `general` | everything | A delegated sub-task |

`plan` and `explore` **cannot write files.** That's enforced by the permission
gate, not by wording in a prompt — so it holds even if the model tries.

---

## When something goes wrong

| Symptom | What's happening | Fix |
|---|---|---|
| Empty reply | Reasoning consumed the whole output budget | It retries once automatically; if it persists, `/reasoning minimal` |
| `could not connect` / `provider error (500)` | The gateway is struggling | It retries with backoff and shows a countdown. `Ctrl+C` stops it |
| A model always 500s | That model is broken at the gateway | `/model` and pick another |
| Replies feel slow | Large context, or high reasoning effort | Check `ctx %` in the footer; `/reasoning low`; `/compact` |
| The model seems confused | Context is full of old noise | `/compact`, or start a fresh session |
| Wrong file edited | — | `/undo` — it restores the files too |
| Anything stranger | — | `/logs` — it says what actually happened |

---

## Tuning it

`~/.config/theta/config.toml`, written with defaults on first run. The knobs
worth knowing:

```toml
[ai]
model = "deepseek-v4.1-flash"
reasoning_effort = "low"        # or use /reasoning
max_retries = 0                 # 0 = keep retrying (good for busy gateways)
timeout_secs = 300              # idle timeout, not a total limit

[compaction]
prune = true                    # drop old tool output so it isn't re-billed
prune_protect_tokens = 40000    # recent output kept verbatim

[ui]
restore = true                  # reopen your last workspace

[behavior]
auto_approve_permissions = false
notify = true                   # bell + desktop notification when a run ends
local_permissions = "allow"     # allow | ask | deny | read-only
```

**`local_permissions`, honestly:** `allow` means it never asks. That's fast and
it's the default. `ask` prompts before anything that writes or runs. `read-only`
is a hard guarantee nothing is touched. If you're working in a repo you care
about, `ask` is worth the small interruption.

---

## Headless — for scripts

```sh
theta --print "list every TODO in src/"      # one turn, print the reply
theta --print --json "count the tests"       # JSONL events, for a pipeline
```

Useful in CI, git hooks, or anywhere you want one answer without a UI.

---

## Custom keybindings and skills

- `/keys` edits keybindings live. Everything is remappable.
- Skills and prompt packs load from `~/.config/theta/skills/`, `./.theta/skills/`
  and `.agents/skills/` automatically — no registration step.
- `AGENTS.md` in a project is read as instructions for that project. This is the
  highest-leverage file: put your conventions in it and stop repeating yourself.

---

## Getting the most out of it

The habits that matter most, in order:

1. **Put your conventions in `AGENTS.md`.** Test commands, style rules, what not
   to touch. Then you never explain it again.
2. **Two panes for anything non-trivial.** Plan in one, build in the other. The
   parallelism is the entire point of a TUI workspace.
3. **Use `plan` before you're sure.** It can't break anything.
4. **Undo freely.** `/undo` restores files. Experimenting is cheap, so experiment.
5. **`!!` for noisy commands.** Keeps the context (and the bill) small.
6. **New topic, new session.** History is re-sent every message.
7. **Check `/logs` before blaming the model.** It's usually the gateway.
8. **Lower `/reasoning` for routine work.** Most edits don't need deep thought.
