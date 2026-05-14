# Design

A single-binary Rust CLI for day-to-day use of Google's Gemini API. Inspired by `aichat`. Target release binary <20 MB; current ~6 MB.

## Principles

- Gemini-only. No provider abstraction layer.
- Roll our own HTTP/API client, no `google-genai` SDK.
- Minimal deps. Pull libraries only where they earn their weight.
- Single binary, single global SQLite DB, filesystem for blobs.

## Scope

In: chat REPL + one-off, sessions (named + ephemeral), roles, aliases, image / TTS / music gen, file attachments, markdown/code rendering, Gemini server-side built-in tools (`google_search`, `url_context`, `code_execution`), built-in local tools (`read_file`, `list_dir`, `fetch_url`, `exec`), user-defined function tools, per-session token/cost tracking, in-terminal image preview (Kitty / iTerm2), `genai init` first-run wizard, `genai gc`, `genai models sync`, `genai audit tail`, tracing logging, unified tool-call policy.

Out (deferred — see [roadmap.md](roadmap.md)): realtime voice (Live API), embeddings as a user feature, RAG, MCP, streaming with tool calls, `exec` sandboxing, Windows.

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
  tool-log.jsonl        # audit log

~/.cache/genai/         # regenerable: rustyline history, etc.
```

Set `GENAI_HOME=/path/to/dir` to override the root — config/data/cache live under that single tree (handy for scratch / test isolation).

## SQLite schema

```
sessions(id, name UNIQUE, model, system_prompt, created_at, updated_at, ephemeral, meta JSON)
messages(id, session_id, seq, turn_id, role, parts JSON, token_count, created_at)
attachments(hash PRIMARY KEY, mime, size, created_at)
message_attachments(message_id, attachment_hash)
```

- `parts` is always an array of typed parts matching Gemini's API shape (text, inlineData, functionCall, functionResponse). Forward-compat for multimodal.
- `messages.turn_id` groups all rows belonging to one logical user turn — for plain chat that's two rows (user + model), for a function-calling exchange it's the user prompt plus the full model/tool back-and-forth. `pop_last_turn` deletes by `turn_id`.
- `sessions.ephemeral = 1` flags temporary REPL sessions; they don't appear in listings and are pruned when the user discards them.
- WAL mode, single open connection per process.
- Attachments addressed by sha256 (16 hex chars). GC = delete blobs not referenced by any `message_attachments`.

## Config resolution chain

CLI flag > active role > session meta > user config > built-in default.

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
        ├── audit.rs      # tool-call audit log + retention
        ├── cli.rs        # clap definitions
        ├── config.rs     # config loading, paths(), GENAI_HOME override, security types
        ├── init.rs       # `genai init` first-run wizard
        ├── output.rs     # write_audio / write_images / expand_path / image_preview / describe_image
        ├── role.rs
        ├── spinner.rs    # animated stderr indicator for silent waits
        ├── ui.rs         # confirm / read_line / read_required / read_secret
        ├── gemini/       # API client: chat, image, tts, types
        ├── session/      # session, db, attachment
        ├── repl/         # chat, commands, complete, dispatch, media, prompt, render, sessions
        ├── models/       # registry + alias resolution + sync (data.toml bundled)
        └── tools/        # builtin, cli_ui, local, policy, process, runner, user
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
libc
imagesize
```

Markdown rendering: hand-rolled, no library.

## Logging

No log output by default. Opt in via `GENAI_LOG=...` (falls back to `RUST_LOG`) using the standard `tracing-subscriber` filter syntax — e.g. `GENAI_LOG=genai=debug` to see API requests, SSE event sizes, tool-loop iterations, registry loads, and policy decisions on stderr. User-facing messages are not affected by the log filter.

## Release profile

```toml
[profile.release]
opt-level = "z"
lto = "fat"
codegen-units = 1
strip = true
panic = "abort"
```

Target: 6–15 MB stripped binary.
