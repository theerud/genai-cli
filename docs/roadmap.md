# Roadmap

The state of work that's been considered, scoped, and deferred. Active deferred items are in priority order — the team can pick up any one without coordination. Track-only notes are observations worth keeping in view but don't have an action.

## Active deferred — bigger work

| | Item | Why deferred |
|---|---|---|
| | **Sandboxing for `exec`** (bubblewrap / landlock / chroot) | Real protection but invasive; pulls in significant Linux specifics and a portability burden |
| | **MCP integration** | Big API surface; comes after our own tool model is fully fleshed out |
| | **Streaming with tool calls** | Current loop is non-streaming. Streaming-with-tools needs a careful state machine for mid-stream tool calls |
| | **Embeddings as a user feature** | No clear UX winner; out until a memory subsystem lands |
| | **RAG / memory subsystem** | Bigger architecture. Current schema leaves room but there's no concrete plan |
| | **Live API (realtime voice)** | Big WebSocket surface; chat is text-only today |
| | **Cross-platform / Windows support** | `#[cfg(unix)]` + `sh -c` assumptions throughout. Explicitly Unix-only |

## Active deferred — smaller work

| | Item | Notes |
|---|---|---|
| | Test coverage for tool runner / exec | Manual smoke testing has been thorough; tests would be insurance |
| | Pricing data sync / drift detection | Discussed; deferred (no source covers Imagen/Lyria/TTS/embed) |
| | DNS-rebinding defense on `fetch_url` | Resolves hostname to IP before deciding; out for v0, literal-host match is enough |
| | Per-role permission profiles | "When role=research, allow `fetch_url` to news.*" — pairs with policy work |
| | Time-bounded rules / capability tokens | "Allow `exec` for 5 minutes." Speculative |
| | `genai security test <prompt>` simulator | Pre-flight check: which rules would fire? Useful for tuning |
| | Richer audit viewer (filter by tool, by result) | `genai audit tail` is enough for now |
| | REPL command autocomplete: arg-aware hints | Today only commands are completed; future: smart param hints |

## Track-only

These don't need action but are worth keeping in view:

- **`gemini-3-family` MALFORMED_FUNCTION_CALL quirk**: prompts that mention a real Gemini server-side tool name (`google_search`) without declaring the tool make the model emit a malformed call attempt. We surface `finish_reason=MALFORMED_FUNCTION_CALL` instead of silent empty output. Watch whether Google fixes it.
- **iTerm2 partial Kitty support**: iTerm2 acks the Kitty graphics protocol but doesn't render. We probe iTerm2 first to sidestep this. If they fix it, we could simplify back to single-probe.
- **Lyria endpoint shape**: uses `generateContent` + `AUDIO` modality without `speechConfig`. If Google changes the request shape, the call will fail loudly (clear error path, no silent failure).
- **Pricing drift in `models/data.toml`**: review every few months or when Google announces price changes.

## Explicit non-goals

- **Auto-curation of model metadata.** `models sync` is intentionally additive over the curated bundled list — pricing and capability labels stay hand-edited.
- **Glob features beyond `*` in policy rules.** No `?`, no character classes, no regex. Keeps the matcher small and predictable.
- **Negation in policy rules.** "Allow except X" is expressed by a higher-priority deny rule.
- **Multi-provider abstraction.** Gemini-only; if you want OpenAI or Anthropic, use a different tool.

## Recent slices (for context)

Shipped in this iteration of work:

- v0 scope (chat, sessions, roles, aliases, media gen, attachments)
- Gemini server-side built-in tools
- Client-side function-tool loop + built-in local tools
- User-defined function tools with `<config_dir>/tools/bin/` PATH injection
- REPL tab completion, `.undo`/`.retry`, `.preview`, `.audit`, `.trust`
- REPL refactor (1300 → ~250 lines per submodule)
- `genai init`, `genai gc`, `genai models sync`, `genai audit tail`
- `tracing` logging behind `GENAI_LOG`
- `GENAI_HOME` override
- Image preview (Kitty + iTerm2 probes via `/dev/tty`)
- Image info line (dimensions + format + size)
- Unified tool-call policy (`[[security.rule]]`) with glob patterns
- Audit log + retention
- Per-session trust state
- Spinner during silent waits
- Loop-mode roles (multi-iteration tool runs under one prompt; `write_file` built-in; `--max-iter`)
- Unified `generate_media` tool (image / speech / music dispatch with auto-path and optional inline preview)

See git log for the full sequence.
