# genai-cli — Design

A single-binary Rust CLI for day-to-day use of Google's Gemini API: REPL chat, one-off prompts, image / TTS / music generation. Inspired by `aichat`. Target release binary <20MB.

## Principles

- Gemini-only. No provider abstraction layer.
- Roll our own HTTP/API client, no `google-genai` SDK.
- Minimal deps. Pull libraries only where they earn their weight.
- Single binary, single global SQLite DB, filesystem for blobs.

## Scope (v1)

In: chat REPL + one-off, sessions (named + ephemeral), roles, aliases, image gen, TTS, music gen, file attachments (input + output), markdown/code rendering, Gemini server-side built-in tools (`google_search`, `url_context`, `code_execution`), client-side local tools (`read_file`, `list_dir`, `fetch_url`, `exec`), per-session token/cost tracking, `genai init` first-run wizard, `genai gc` for orphan blob cleanup.
Out (deferred): realtime voice (Live API), embeddings as a user feature, user-defined function tools, RAG, MCP, streaming with tool calls.

Image default: prefer `gemini-2.5-flash-image` (nano-banana) over Imagen.

## Auth

Resolution order: process env (default `GEMINI_API_KEY`, name overridable via `api_key_env`) → `./.env` → `~/.config/genai/.env` → `api_key` field in `config.toml`.

## On-disk layout

```
~/.config/genai/
  config.toml
  aliases.toml          # optional, can also live inline in config.toml
  roles/
    <name>.toml
  models.toml           # optional user overlay over bundled registry

~/.local/share/genai/
  data.db               # single SQLite DB (sessions, messages, attachments index)
  attachments/<hash>.*  # content-addressed blobs

~/.cache/genai/         # regenerable
```

## SQLite schema (sketch)

```
sessions(id, name UNIQUE, model, system_prompt, created_at, updated_at, meta JSON)
messages(id, session_id, seq, role, parts JSON, token_count, created_at)
attachments(hash PRIMARY KEY, mime, size, created_at)
message_attachments(message_id, attachment_hash)
```

- `parts` always an array of typed parts, matching Gemini's API shape. Forward-compat for multimodal.
- WAL mode. Single open connection per process.
- Attachments addressed by sha256 (16 hex chars). GC = delete blobs not referenced by any `message_attachments`.

## Roles

TOML files under `~/.config/genai/roles/<name>.toml`. Bundle model + system_prompt + params + optional capability defaults.

Supported role fields: `model`, `system_prompt`, `temperature`, `max_tokens`, `thinking_level`, `output_dir`, `tools`.

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

User-defined function tools (`~/.config/genai/tools/*.toml`) and MCP are explicitly out of scope for v0.

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

Bundled `models.toml` (embedded with `include_str!`) holds the curated list: id, capabilities, context window, pricing, thinking levels, status. User overlay in `~/.config/genai/models.toml` merges on top.

Dev tool `genai-models-gen` (workspace member, not in release binary) diffs `models.list` API against bundled registry and reports new/changed/deprecated models to stderr for manual curation. No auto-writes.

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
| `.tools [name]` | List or toggle Gemini server-side built-in tools |
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
4. Stream Gemini API.
5. **On success only:** single transaction inserts user message + assistant message + attachments, updates `sessions.updated_at`.
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
├── genai/                # main binary
│   └── src/
│       ├── main.rs
│       ├── cli.rs        # clap definitions
│       ├── config.rs
│       ├── role.rs
│       ├── gemini/       # API client: chat, image, tts, embed, types
│       ├── session/      # session, db, attachment
│       ├── repl/         # main loop, commands, prompt, render
│       ├── models/       # registry + alias resolution (data.toml bundled)
│       └── error.rs
└── genai-models-gen/     # dev-only tool
```

## Dependencies (planned)

```
tokio (rt-multi-thread, macros, fs, io-util, signal)
reqwest (rustls-tls, stream, json; no default features)
serde, serde_json
toml
clap (derive)
rusqlite (bundled)
rustyline
crossterm
directories
sha2
syntect (curated syntax set)
thiserror, anyhow
futures-util
```

Markdown rendering: hand-rolled, no library.

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
