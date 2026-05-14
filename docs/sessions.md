# Sessions

A *session* is a named (or anonymous) conversation persisted to a single SQLite database. Sessions decouple "this chat" from "all chats" and let you resume work later, switch context, or replay.

## Lifecycle

```
genai                      → REPL, no session (history in memory only)
genai -s research          → REPL, named session "research" (created if missing)
> .session start           → begin an ephemeral session
> hi
> .session save my-notes   → persist the ephemeral as "my-notes"
> .session switch other    → switch to a different saved session
> .session drop            → leave session mode, history cleared
> .session list            → show every saved session
> .session delete my-notes → remove from DB
> .session export my-notes → JSONL to stdout
```

CLI subcommands for cross-session work:

```
genai sessions list
genai sessions delete <name>
genai sessions export <name> [-o path]
```

## Named vs ephemeral

- **Named**: persisted with a user-chosen identifier. Shown in `.session list`.
- **Ephemeral**: persisted but flagged `ephemeral = 1` in the DB. Hidden from listings. Useful for "let me think about something quick" without polluting your saved list.

The CLI tracks unsaved ephemeral state and prompts on exit:

```
Temporary session has unsaved turns. [s]ave as / [d]iscard / [c]ancel?
```

The same prompt fires on `.session switch`, `.clear`, and the REPL's `Ctrl-D` / `.exit`.

## Persistence contract

A turn is committed atomically:

1. The user message + any attachments.
2. The full assistant/tool exchange — one row for plain chat, multiple rows for a function-calling exchange, all sharing one `turn_id`.

Failed turns (network error, Ctrl-C cancel during streaming) leave **no** rows behind. The on-screen prompt is preserved by rustyline for up-arrow retry.

DB-commit failures (full disk, locked DB) downgrade to a stderr `warning:` line; the in-memory history still advances so the model sees the conversation it just produced.

## `.undo` and `.retry`

- `.undo` removes the last `turn_id` from both the DB (if a session is active) and in-memory history. Works correctly for tool exchanges — the whole multi-row exchange is one turn.
- `.retry` is `.undo` followed by re-asking the last user prompt.

Without an active session, `.undo` falls back to "drop everything after the most recent user-role message in history."

## History inheritance

When you `.session start` while you have anonymous history, you're prompted:

```
Include 3 previous turn(s) in this temporary session? [Y/n]
```

If yes, those turns are inserted into the new session preserving their `turn_id` groupings — a function-calling exchange in anonymous history becomes one turn in the new session, not several.

## Tokens and cost

Each session tracks cumulative prompt / output tokens and an estimated USD cost from the bundled pricing in `models/data.toml`. View with `.info` in the REPL.

Pricing is curated by hand (see [models.md](models.md)); cost numbers should be treated as estimates, not invoices.

## JSONL export

`.session export <name>` or `genai sessions export <name>` emits one JSON line per message:

```jsonl
{"type":"session","name":"research","model":"gemini-2.5-flash","system_prompt":null}
{"seq":1,"role":"user","parts":[{"type":"text","text":"..."}],"created_at":"..."}
{"seq":2,"role":"model","parts":[{"type":"text","text":"..."}],"created_at":"..."}
```

`inlineData` parts include `mime_type` and `size` but not the bytes — the raw blobs live in `<data_dir>/attachments/<hash>.<ext>`.

## On-disk layout

```
<data_dir>/
  data.db                # sessions, messages, attachments index, schema version
  attachments/<hash>.*   # content-addressed blobs (sha256, 16 hex chars)
```

Schema version: `3` (as of writing). Migrations run automatically on `Database::open` if the file's version is lower.

`genai gc` removes attachment blobs no longer referenced by any message.
