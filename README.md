# genai-cli

A single-binary CLI for daily use of Google's Gemini API: chat REPL, one-off prompts, file attachments, image / TTS / music generation, sessions, roles, aliases.

Inspired by [aichat](https://github.com/sigoden/aichat) but Gemini-only and dependency-light. Release binary ≈6 MB.

## Install

Requires Rust nightly (edition 2024) and OpenSSL development headers on the host (for `native-tls`).

```bash
git clone <repo> genai-cli && cd genai-cli
cargo build --release -p genai-cli
# binary at target/release/genai
```

To install system-wide:

```bash
cargo install --path crates/genai-cli
```

## API key

Set your Gemini key one of these ways (first match wins):

1. **Process env**: `export GEMINI_API_KEY=...`
2. **`./.env`** in the current working directory
3. **`~/.config/genai/.env`**
4. **`api_key = "..."`** field in `config.toml` (back-compat; least preferred)

`.env` files use the standard `KEY=VALUE` format, with optional `"..."` / `'...'` quoting and `export ` prefix support.

To pull from a differently-named env var, set in `config.toml`:

```toml
api_key_env = "GEMINI_PERSONAL_KEY"
```

## Quick start

```bash
# One-off chat (streaming to stdout)
genai "Explain monads in two sentences"

# Pick a model
genai -m gemini-2.5-pro "Walk me through the borrow checker"

# REPL
genai

# Resume / create a session
genai -s research "I'm reading the Raft paper. Track context for me."
genai -s research "Recap leader election."

# Attach a file as input
genai -f screenshot.png "What's broken in this UI?"

# Generate an image (nano-banana / Gemini Image — Imagen also supported)
genai -m gemini-2.5-flash-image -o cat.png "a watercolor cat reading a book"

# Edit an image
genai -m gemini-2.5-flash-image -f cat.png -o cat-blue.png "make the cat blue"

# Speak text
genai -m gemini-2.5-flash-preview-tts -o hello.wav "Hello there."

# Generate music
genai -m lyria-3-clip-preview -o tune.mp3 "lofi piano"
```

Pipe-friendly: when stdout isn't a TTY, output is plain text (no ANSI, no streaming flushes).

## REPL

```
$ genai
genai-cli — model: gemini-2.5-flash
Type .help for commands, .exit or Ctrl-D to quit.
> hello
Hi! How can I help...
> .model gemini-2.5-pro
model: gemini-2.5-pro
> .help
```

Prompt markers:

| | |
|---|---|
| `> ` | no role, no session |
| `*> ` | anonymous chat in a session |
| `myrole> ` | role active, no session |
| `*myrole> ` | role + session |

### Dot-commands

| Command | Purpose |
|---|---|
| `.help`, `.exit` / `.quit` / Ctrl-D | basics |
| `.info` | model / session / role / params summary |
| `.clear` | wipe in-memory history, drop session |
| `.model [id\|-]` | show / switch / reset chat model |
| `.set <key> <value>` | `temperature`, `max-tokens`, `thinking` |
| `.file <path>...` | queue file(s) for next message |
| `.edit` | compose next message in `$EDITOR` |
| `.session` | show current session state |
| `.session start` | begin an ephemeral session |
| `.session save <name>` | persist the ephemeral session under a name |
| `.session switch <name\|id>` | resume a saved session |
| `.session rename <name>` | rename current session |
| `.session list` | list sessions with IDs |
| `.session drop` | discard current ephemeral session |
| `.session delete <name\|id>` | delete a saved session |
| `.session export <name\|id>` | export current session as JSONL |
| `.role [name\|list\|-]` | switch / list / clear role |
| `.tools [name]` | list or toggle Gemini server-side built-in tools |
| `.undo` | drop the last completed turn |
| `.retry` | re-run the previous user prompt |
| `.image [-m MODEL] [-o PATH] [-f FILE] "prompt"` | generate an image |
| `.tts [-m MODEL] [-v VOICE] [-o PATH] "text"` | speech synthesis |
| `.music [-m MODEL] [-o PATH] "prompt"` | music generation |

Ctrl-C during a streaming response cancels cleanly without polluting session history. Up-arrow recalls your prompt so you can edit + resend.

## CLI subcommands

| Command | What it does |
|---|---|
| `genai models list` | bundled + user-overlay models, grouped by capability |
| `genai sessions list` | all stored sessions |
| `genai sessions delete <name>` | delete a session |
| `genai sessions export <name> [-o PATH]` | export session as JSONL (stdout if `-o -` or omitted) |
| `genai gc` | remove attachment blobs no longer referenced by any message |
| `genai init [--force]` | first-run wizard to write `config.toml` |

## Config

`~/.config/genai/config.toml`:

```toml
# api_key_env defaults to GEMINI_API_KEY
# api_base = "https://generativelanguage.googleapis.com"

[model.chat]
default = "gemini-2.5-flash"
temperature = 0.7
# max_tokens = 8192
# system_prompt = "You are concise."

[model.image]
default = "gemini-2.5-flash-image"

[model.tts]
default = "gemini-2.5-flash-preview-tts"
voice = "Kore"

[model.embed]
default = "gemini-embedding-2"

[repl]
markdown = true
color = true
# history_size = 10000

# Aliases — named bundles of model + per-model params, usable anywhere a
# model id is accepted.
[aliases.pro-high]
model = "gemini-2.5-pro"
thinking_level = "high"

[aliases.fast]
model = "gemini-2.5-flash-lite"
temperature = 0.3
```

`thinking_level` maps to thinking budgets: `off`, `low`, `medium`, `high`, `dynamic` (or `auto`).

### Roles

Drop TOML files in `~/.config/genai/roles/`:

```toml
# ~/.config/genai/roles/coding.toml
model = "gemini-2.5-pro"
system_prompt = """
You are a senior Rust engineer. Answer concretely with code where it helps.
"""
temperature = 0.4
thinking_level = "high"
```

Then:

```bash
genai -r coding "explain Pin<&mut T>"
genai -r coding -s rust-notes  # role + session
```

Inside the REPL: `.role coding` to switch, `.role -` to clear, `.role list` to show available.

**Capability rule:** if the role's model is output-only (Imagen, TTS, Lyria), bare REPL chat falls back to the default chat model with no role system prompt. The role still configures `.image` / `.tts` / `.music` invocations.

Roles may opt into tools, mixing Gemini server-side built-ins and client-side local tools in one list:

```toml
# ~/.config/genai/roles/research.toml
model = "gemini-2.5-pro"
system_prompt = "Cite sources when relevant."
tools = ["google_search", "url_context"]

# ~/.config/genai/roles/sysadmin.toml
model = "gemini-2.5-pro"
system_prompt = "Inspect the user's machine and answer with concrete evidence."
tools = ["read_file", "list_dir", "fetch_url", "exec"]
```

Available tools:

| Tool | Kind | Notes |
|---|---|---|
| `google_search` | Gemini built-in | Web search |
| `url_context` | Gemini built-in | Fetch + ground on URLs server-side |
| `code_execution` | Gemini built-in | Sandboxed Python |
| `read_file` | local | Up to 256 KB of text |
| `list_dir` | local | Up to 200 entries |
| `fetch_url` | local | http(s) GET, up to 1 MB |
| `exec` | local | `sh -c …`; **prompts for confirmation each call** |

When any local tool is active, streaming output is disabled and the model is allowed to call tools up to 8 times before producing a final answer. Each call prints a `[tool] …` line on stderr.

### User-defined tools

Drop a TOML file in `<config_dir>/tools/<name>.toml`:

```toml
description = "Show recent git commits as 'hash subject' lines."
command = ["git", "-C", "{{path}}", "log", "--oneline", "-n", "{{limit}}"]
timeout_secs = 10
# confirmation = true     # prompt y/N before each call

[args.path]
type = "string"
description = "Path to the working tree."
required = true

[args.limit]
type = "integer"
default = 10
```

Argument types: `string`, `integer`, `number`, `boolean`. `{{name}}` placeholders in `command` are replaced with the validated arg values. Tools can't shadow built-in names. Scripts dropped in `<config_dir>/tools/bin/` are reachable by user tools — that dir is prepended to `PATH` only for user-tool execution.

## Sessions & storage

Everything except attachments lives in a single SQLite DB:

```
~/.local/share/genai/
  data.db                   # sessions, messages, attachment index
  attachments/<hash>.<ext>  # content-addressed blobs
```

- Sessions persist only complete turns. Failed or Ctrl-C-cancelled turns leave no trace (so a half-rendered assistant response never feeds back into the next API call).
- Attachments are sha256-hashed and deduplicated. `genai gc` removes blobs no longer referenced by any message.
- Sessions are exportable as JSONL.

## Models registry

The CLI ships with a curated list of known Gemini models in `crates/genai-cli/src/models/data.toml`. Override or extend locally by dropping entries into `~/.config/genai/models.toml` (same schema, merged on top of the bundled list).

To inspect drift between the curated list and the live API:

```bash
GEMINI_API_KEY=... cargo run -p genai-models-gen
# Reports new / missing / changed entries to stderr. Does not write to data.toml.
```

## Known limitations

- **TTS** assumes 16-bit mono PCM @ 24 kHz when wrapping into WAV (matches current Gemini output). Multi-channel TTS would need `pcm16_to_wav` adjustment.
- **Markdown rendering is line-buffered.** Output appears at line granularity, not character granularity. Trade-off for streaming markdown without flicker.
- **Realtime voice (Gemini Live API)** is not implemented. The chat REPL is text-only.
- **Embeddings as a user feature, RAG, and user-defined function tools** are not yet implemented. Gemini server-side built-ins (`google_search`, `url_context`, `code_execution`) and a fixed set of local tools (`read_file`, `list_dir`, `fetch_url`, `exec`) are wired up via roles and the `.tools` REPL command.
- **Streaming is disabled** while a local tool is active. The non-streaming function-call loop is simpler; streaming-with-tools is deferred.
- **Lyria** worked in the smoke test but is preview and may change request shape; if it breaks you'll see the server error verbatim — adjust `generate_music` in `gemini/tts.rs` if needed.

## Project layout

```
genai-cli/
├── crates/
│   ├── genai-cli/          # main binary `genai`
│   └── genai-models-gen/   # dev tool, not shipped
├── DESIGN.md               # design rationale
└── README.md               # this file
```

See [`DESIGN.md`](DESIGN.md) for architectural decisions and the reasoning behind them.

## License

MIT — see [`LICENSE`](LICENSE).
