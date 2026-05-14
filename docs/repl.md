# REPL

`genai` with no prompt drops you into an interactive session.

```
$ genai
genai-cli — model: gemini-2.5-flash
Type .help for commands, .exit or Ctrl-D to quit.
>
```

## Prompt markers

| Marker | Meaning |
|---|---|
| `> ` | no role, no session |
| `*> ` | anonymous chat in a session |
| `myrole> ` | role active, no session |
| `*myrole> ` | role + session |

Long role / session names are truncated to 16 chars with an ellipsis.

## Dot-commands

| Command | Purpose |
|---|---|
| `.help`, `.exit` / `.quit` / Ctrl-D | basics |
| `.info` | model / session / role / params / usage summary |
| `.clear` | wipe in-memory history, drop session |
| `.model [id\|-]` | show / switch / reset chat model |
| `.set <key> <value>` | adjust `temperature`, `max-tokens`, `thinking` |
| `.file <path>...` | queue file(s) for next message |
| `.edit` | compose next message in `$EDITOR` |
| `.session` | show current session state |
| `.session start` | begin an ephemeral session |
| `.session save <name>` | persist the ephemeral session under a name |
| `.session switch <name\|id>` | resume a saved session |
| `.session rename <name>` | rename current session |
| `.session list` | list saved sessions with IDs |
| `.session drop` | discard current ephemeral session |
| `.session delete <name\|id>` | delete a saved session |
| `.session export <name\|id>` | export current session as JSONL |
| `.role [name\|list\|-]` | switch / list / clear role |
| `.tools [list\|name]` | list or toggle tools (built-in, local, user-defined) |
| `.preview <path>` | render an image inline (Kitty / iTerm2) |
| `.audit [N]` | show the last N audit-log entries (default 20) |
| `.trust [list\|clear\|drop <name>]` | inspect / revoke per-session tool trust |
| `.undo` | drop the last completed turn (history + DB) |
| `.retry` | re-run the previous user prompt |
| `.image [-m MODEL] [-o PATH] [-f FILE] "prompt"` | image generation |
| `.tts [-m MODEL] [-v VOICE] [-o PATH] "text"` | TTS |
| `.music [-m MODEL] [-o PATH] "prompt"` | music generation |

Ctrl-C during a streaming response cancels cleanly without polluting session history. Up-arrow recalls your prompt so you can edit and resend.

## Tab completion

- Type `.` then Tab → list of dot-commands
- `.session sw<Tab>` → `switch`
- `.session switch <Tab>` → existing session names + `#<id>` references
- `.role <Tab>` → role files in `<config_dir>/roles/`
- `.model <Tab>` → chat-capable model IDs + aliases
- `.tools <Tab>` → known tool names (built-in + local + user-defined)
- `.file <Tab>`, `.preview <Tab>` → filesystem paths

## In-terminal image preview

When you generate an image or run `.preview <path>`, the CLI tries to render it inline using the terminal's native graphics protocol.

Supported:

- **iTerm2 inline images** — iTerm2, WezTerm
- **Kitty graphics** — Kitty, Ghostty, WezTerm, foot

Detection is a live query/response handshake against `/dev/tty` so it works through ssh and tmux (tmux requires `set -g allow-passthrough on`). Failure mode is silent — if the terminal doesn't answer the probe, the image is saved and that's it.

Override the auto-detection in `config.toml`:

```toml
[output]
image_preview = "auto"       # default; probe and pick
# image_preview = "iterm2"   # force
# image_preview = "kitty"    # force
# image_preview = "off"      # never preview
```

Force a protocol if your terminal advertises Kitty support without actually rendering correctly (recent iTerm2 builds do this).

## Spinner

A small animated indicator shows on stderr during silent waits — non-streaming LLM calls, tool execution, image / TTS / music generation. Skipped automatically when stderr isn't a TTY (piped / redirected). Cleans up its line before any real output prints.

## Session UX edges

- Starting a new REPL while you have anonymous chat history (from before `.session start`) prompts you to inherit those turns into the new session.
- Trying to exit (or `.session switch` / `.clear`) while an ephemeral session has unsaved turns prompts: **s**ave as / **d**iscard / **c**ancel.
- See [sessions.md](sessions.md) for the full lifecycle.
