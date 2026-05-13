# genai-cli — Design

A single-binary Rust CLI for day-to-day use of Google's Gemini API: REPL chat, one-off prompts, image / TTS / music generation. Inspired by `aichat`. Target release binary <20MB.

## Principles

- Gemini-only. No provider abstraction layer.
- Roll our own HTTP/API client, no `google-genai` SDK.
- Minimal deps. Pull libraries only where they earn their weight.
- Single binary, single global SQLite DB, filesystem for blobs.

## Scope (v1)

In: chat REPL + one-off, sessions (named + ephemeral), roles, aliases, image gen, TTS, music gen, file attachments (input + output), markdown/code rendering, Gemini server-side built-in tools (`google_search`, `url_context`, `code_execution`), built-in local tools (`read_file`, `list_dir`, `fetch_url`, `exec`), user-defined local tools (`<config_dir>/tools/*.toml`), per-session token/cost tracking, `genai init` first-run wizard, `genai gc` for orphan blob cleanup.
Out (deferred): realtime voice (Live API), embeddings as a user feature, RAG, MCP, streaming with tool calls.

Image default: prefer `gemini-2.5-flash-image` (nano-banana) over Imagen.

## Auth

Resolution order: process env (default `GEMINI_API_KEY`, name overridable via `api_key_env`) → `./.env` → `~/.config/genai/.env` → `api_key` field in `config.toml`.

## On-disk layout

```
~/.config/genai/
  config.toml           # aliases live inline as [aliases.NAME] tables
  roles/
    <name>.toml
  tools/
    <name>.toml         # user-defined function tools
    bin/                # scripts reachable from user tools (prepended to PATH)

~/.local/share/genai/
  data.db               # single SQLite DB (sessions, messages, attachments index)
  attachments/<hash>.*  # content-addressed blobs
  models.toml           # synced overlay over the bundled registry (managed by `genai models sync`)

~/.cache/genai/         # regenerable: rustyline history, etc.
```

Set `GENAI_HOME=/path/to/dir` to override the root: config/data/cache then live under that single tree (handy for scratch / test isolation).

## SQLite schema (sketch)

```
sessions(id, name UNIQUE, model, system_prompt, created_at, updated_at, ephemeral, meta JSON)
messages(id, session_id, seq, turn_id, role, parts JSON, token_count, created_at)
attachments(hash PRIMARY KEY, mime, size, created_at)
message_attachments(message_id, attachment_hash)
```

- `parts` is always an array of typed parts matching Gemini's API shape (text, inlineData, functionCall, functionResponse). Forward-compat for multimodal.
- `messages.turn_id` groups all rows belonging to one logical user turn — for plain chat that's two rows (user + model), for a function-calling exchange it's the user prompt plus the full model/tool back-and-forth. `pop_last_turn` deletes by `turn_id`.
- `sessions.ephemeral = 1` flags temporary REPL sessions; they don't appear in listings and are pruned when the user discards them.
- WAL mode. Single open connection per process.
- Attachments addressed by sha256 (16 hex chars). GC = delete blobs not referenced by any `message_attachments`.

## Roles

TOML files under `~/.config/genai/roles/<name>.toml`. Bundle model + system_prompt + params + optional capability defaults.

Supported role fields: `model`, `system_prompt`, `temperature`, `max_tokens`, `thinking_level`, `tools`.

- CLI: `genai -r <role>` or `-r <role>` then drops to REPL.
- REPL: `.role <name>` to switch, `.role -` to clear, `.role` lists.
- **Orthogonal to sessions.** Role is a transient overlay on top of session config. A session created under a role inherits the role's settings as its defaults; subsequent role switches don't mutate the session.
- **Capability rule for REPL chat:**
  - Chat-capable role → bare prompts use role's model + system prompt.
  - Output-only role (Imagen, TTS, Lyria) → bare prompts fall back to default chat model with no role system prompt. `.image`/`.tts`/etc. use the role's model.

### Tools

Two kinds of tools share the same role-level `tools = [...]` list and the same `.tools` REPL command:

- **Gemini server-side built-ins** (`google_search`, `url_context`, `code_execution`) — run inside Gemini's infrastructure; we just declare them on the request.
- **Local (client-side) tools** — invoked by Gemini via function calling. Built-in to the binary in v0:
  - `read_file`, `list_dir`, `fetch_url`: read-only, no confirmation.
  - `exec`: side-effecting, **prompts for confirmation each call** (auto-denied when stdin is not a TTY).

When any local tool is enabled, the chat path switches from streaming to a non-streaming function-call loop. Streaming with tool calls is deferred.

#### Function-call loop

1. Send `generateContent` with `tools.functionDeclarations` for the local tools (and any built-ins).
2. If the response contains `functionCall` parts, execute each tool locally and append a single `user` message holding all `functionResponse` parts. Repeat.
3. Stop when the model returns a text-only response, or bail at `MAX_TOOL_ITERATIONS` (8).
4. Persist the full exchange (user → model+tool back-and-forth → final text) atomically in a single transaction.

#### User-defined local tools

Drop a TOML file in `<config_dir>/tools/<name>.toml`. The filename stem is the tool name; it must not shadow a built-in (`read_file`, `list_dir`, `fetch_url`, `exec`) — shadows are rejected with a warning at load time.

```toml
description = "Show recent git commits as 'hash subject' lines."
command = ["git", "-C", "{{path}}", "log", "--oneline", "-n", "{{limit}}"]
timeout_secs = 10
confirmation = false  # set true to prompt y/N before each call

[args.path]
type = "string"           # string | integer | number | boolean
description = "Path to the working tree."
required = true

[args.limit]
type = "integer"
default = 10
```

- Execution is **argv-only** (no `sh -c`); `command` is a `Vec<String>` with `{{name}}` placeholders substituted from validated args.
- Type coercion is lenient (e.g. `"20"` for an integer is accepted).
- `<config_dir>/tools/bin/` is prepended to `PATH` when executing user-defined tools, letting helper scripts live alongside their `.toml`. Built-in tools keep the caller's PATH untouched.
- The registry is built once per process: edits require restart.

MCP is explicitly out of scope for v0.

## Aliases

Named model + model-level params (no system prompt). Usable anywhere a model ID is expected. Resolution: alias lookup first, then raw model ID against registry, else error.

```toml
[aliases.pro-high]
model = "gemini-3.1-pro-preview"
thinking_level = "high"
```

## Config resolution chain

CLI flag > active role > session meta > user config > built-in default.

## Models registry

Bundled `data.toml` (embedded with `include_str!`) holds the curated list: id, capabilities, context window, pricing, thinking levels, status. `genai models sync` refreshes a synced overlay at `<data_dir>/models.toml` from the live `models.list` API, covering entries the bundled list does not yet know about (preview models, new family releases). Bundled entries stay canonical because they carry curated fields the API does not return (pricing, capability labels, thinking levels).

The synced overlay is fully managed by the sync command — hand-edits get clobbered on the next run. To add a custom model id the API does not know about, define an alias instead; aliases pass any model id through to the API (unknown ids get a warning, never a hard block).

## REPL

Rustyline-based. Prompt shape: `>`, `*>` (session, no role), `myrole>` (role, no session), `*myrole>` (both). Long-name handling deferred — current scheme is fixed-width and never misleading.

### Dot-commands (v1)

| Command | Purpose |
|---|---|
| `.help`, `.exit` / `.quit` (Ctrl-D) | Basics |
| `.info` | Show model / session / role / token usage / cost |
| `.clear` | End current session, start anonymous |
| `.session` | Show current session state |
| `.session start` | Begin an ephemeral session |
| `.session save <name>` | Persist current ephemeral session under a name |
| `.session switch <name-or-id>` | Resume a saved session |
| `.session rename <name>` | Rename the current session |
| `.session list` | List sessions with IDs |
| `.session drop` | Discard the current ephemeral session |
| `.session delete <name-or-id>` | Delete a saved session |
| `.session export <name-or-id>` | Export as JSONL |
| `.role [name]`, `.role list`, `.role -` | Role control |
| `.model [name]`, `.model -` | Switch chat model / reset |
| `.set <key> <value>` | Adjust params (temperature, max-tokens, ...) |
| `.file <path>...` | Queue attachments for next message |
| `.edit` | Open `$EDITOR` for multi-line prompt |
| `.tools [list\|name]` | List or toggle tools (built-in Gemini, built-in local, user-defined) |
| `.undo` | Drop the last completed turn from the session |
| `.retry` | Re-run the previous user prompt |
| `.image [-m model] [-o path] "prompt"` | Image generation |
| `.tts [-m model] [-v voice] [-o path] "text"` | TTS |
| `.music [-m model] [-o path] "prompt"` | Music (Lyria) |

Convention: `<cmd> -` to reset / clear, mirrored across `.role` and `.model`. Session resets use the explicit `start`/`drop` subcommands.

Ephemeral session flow: `.session start` creates an unsaved session; leaving it (`.session switch`, `.clear`, or REPL exit) prompts to save, discard, or cancel when there are unsaved turns. New REPL starts may also offer to inherit anonymous history into a fresh session.

## Turn lifecycle

1. Read input, dispatch dot-command or chat turn.
2. Resolve effective config via precedence chain.
3. Build request from in-memory session history + pending attachments.
4. Stream Gemini API (plain chat) **or** run the function-call loop (when any local tool is active).
5. **On success only:** single transaction inserts the user message plus the full assistant/tool exchange (one row for plain chat, multiple for a tool turn — all sharing one `turn_id`) and attachments, updates `sessions.updated_at`.
6. On failure or Ctrl-C cancel: nothing persisted. Rustyline history still has the input for up-arrow retry.

Rationale: a truncated assistant response in persisted history would be sent back to the API as context and confuse the model. Only complete turns commit.

## Output rendering

Streaming, line-buffered. Pipeline of swappable renderers behind a `Renderer` trait:

1. `PlainRenderer` — raw passthrough.
2. `ColorRenderer` — plain text + syntect-highlighted code blocks (curated language set to limit binary size).
3. `MarkdownRenderer` — line-level markdown state machine + syntect for code.

Build order is also ship order — each step independently usable.

Non-TTY stdout (piped) → disable streaming, single buffered write, no ANSI. `isatty(stdout)` check.

Image/audio output paths: required via `-o <path>`, or `-o -` for stdout (binary to stdout; all progress/info to stderr). REPL may prompt if missing.

## Project layout

Cargo workspace:

```
crates/
└── genai-cli/            # main binary (name: `genai`)
    └── src/
        ├── main.rs       # top-level entry, error formatter, tracing init
        ├── cli.rs        # clap definitions
        ├── config.rs     # config loading, paths(), GENAI_HOME override
        ├── init.rs       # `genai init` first-run wizard
        ├── output.rs     # shared write_audio / write_images / expand_path
        ├── role.rs
        ├── ui.rs         # confirm / read_line / read_required / read_secret
        ├── gemini/       # API client: chat, image, tts, types
        ├── session/      # session, db, attachment
        ├── repl/         # chat, commands, complete, dispatch, media, prompt, render, sessions
        ├── models/       # registry + alias resolution + sync (data.toml bundled)
        └── tools/        # builtin, cli_ui, local, process, runner, user
```

## Dependencies

```
tokio (rt-multi-thread, macros, signal)
reqwest (native-tls, stream, json, http2; no default features)
serde, serde_json
toml
clap (derive)
rusqlite (bundled)
rustyline
directories
sha2
syntect (default-fancy; no default features)
anyhow
futures-util, async-stream, bytes
base64
tracing, tracing-subscriber (fmt + env-filter, no default features)
```

Markdown rendering: hand-rolled, no library.

## Logging

No log output by default. Opt in via `GENAI_LOG=...` (falls back to `RUST_LOG`) using the standard `tracing-subscriber` filter syntax — e.g. `GENAI_LOG=genai=debug` to see API requests, SSE event sizes, tool-loop iterations, and registry loads on stderr. User-facing messages are not affected by the log filter; they stay on the usual stderr/stdout paths.

## Release profile

```toml
[profile.release]
opt-level = "z"
lto = "fat"
codegen-units = 1
strip = true
panic = "abort"
```

Target: 10–15MB stripped binary.
