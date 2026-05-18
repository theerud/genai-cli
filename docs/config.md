# Configuration reference

All config lives under `~/.config/genai/` by default. Override the entire root with `GENAI_HOME=/some/path` and the CLI looks at `$GENAI_HOME/{config,data,cache}/` instead.

## API key

First match wins:

1. **Process env**: `GEMINI_API_KEY=...` (override the variable name via `api_key_env` in `config.toml`)
2. **`./.env`** in the current working directory
3. **`<config_dir>/.env`**
4. **`api_key = "..."`** field in `config.toml` (least preferred — keep it out of dotfiles)

`.env` files use the standard `KEY=VALUE` format with optional `"..."` / `'...'` quoting and `export ` prefix support.

## Environment variables

| Variable | Effect |
|---|---|
| `GENAI_HOME` | Override the config / data / cache root. Useful for scratch / test isolation. |
| `GENAI_LOG` (falls back to `RUST_LOG`) | `tracing` filter for debug output to stderr. Examples: `genai=debug`, `info,genai::gemini=trace`. Empty/unset means no log output. |
| `GEMINI_API_KEY` (or whatever `api_key_env` names) | API key. |

## `config.toml`

`genai init` writes a working baseline. Reference of every key:

```toml
# Optional: name the env variable to read the API key from.
# api_key_env = "GEMINI_API_KEY"

# Optional: override the Gemini API host.
# api_base = "https://generativelanguage.googleapis.com"

# Optional: API key inline (least preferred).
# api_key = "..."

# ----- Model defaults -----

[model.chat]
default = "gemini-2.5-flash"
# temperature = 0.7
# max_tokens = 8192
# system_prompt = "You are concise."

# ----- Media defaults (image / speech / music) -----
# Used by `generate_media` and by `genai -m <media-model>` one-shots.
# Roles can override per field via their own [media] table.

[media]
image  = "gemini-2.5-flash-image"
speech = "gemini-2.5-flash-preview-tts"
music  = "lyria-3-clip-preview"

# (TTS voice still lives at [model.tts].voice until multi-speaker lands.)
[model.tts]
voice = "Kore"

[model.embed]
default = "gemini-embedding-2"

# Legacy: [model.image].default and [model.tts].default still work as a
# fallback for [media].image / [media].speech and emit a one-time
# tracing::warn pointing at the new shape. Plan to remove once the
# project hits 1.0.

# ----- REPL ergonomics -----

[repl]
markdown = true              # render ANSI-colored markdown to a TTY
color = true                 # syntax-highlight fenced code blocks
# history_size = 10000

# ----- Output -----

[output]
# In-terminal image preview after .image / image generation.
# "auto" probes the terminal; "iterm2"/"kitty" force a protocol; "off" disables.
image_preview = "auto"
# image_dir = "~/Pictures/genai"
# audio_dir = "~/Music/genai"

# ----- Aliases -----
# Named bundles of (model, per-model params) usable anywhere a model id is
# expected.

[aliases.pro-high]
model = "gemini-2.5-pro"
thinking_level = "high"

[aliases.fast]
model = "gemini-2.5-flash-lite"
temperature = 0.3

# ----- Security: tool-call policy -----
# See docs/tools.md#policy for full semantics.

[[security.rule]]
tool = ["read_file", "list_dir"]
arg = "path"
patterns = ["*/.ssh/*", "*/.aws/*", "*/.gnupg/*", "*/.netrc"]
decision = "deny"
priority = 100

[[security.rule]]
tool = "fetch_url"
arg = "url"
patterns = [
    "http://localhost*", "https://localhost*",
    "http://127.*", "http://10.*", "http://192.168.*",
    "*169.254.169.254*",
]
decision = "deny"
priority = 100

# ----- Security: audit log -----

[security.audit]
enabled = true
max_lines = 5000             # soft cap; trims in place at +10%
```

## Roles

Each role lives in `<config_dir>/roles/<name>.toml` and bundles a model, system prompt, params, and tools. Loaded via `-r <name>` on the CLI or `.role <name>` in the REPL.

```toml
# ~/.config/genai/roles/researcher.toml
model = "gemini-2.5-pro"
system_prompt = "You are a research agent..."
tools = ["google_search", "url_context", "fetch_url", "write_file"]

# Optional: drive multi-step tool workflows under one user prompt.
mode = "loop"                  # "chat" (default) | "loop"
max_iterations = 20            # default 8; --max-iter overrides per invocation

# Per-role overrides for generate_media. Any field set here wins over
# the global [media] table in config.toml.
[media]
image  = "imagen-4.0-generate-001"
# speech = "gemini-2.5-pro-preview-tts"
# music  = "lyria-3-pro-preview"
```

In `loop` mode only the user prompt and the final assistant text are kept in session history — intermediate function calls and tool responses stay out of future context. See [tools.md#loop-mode](tools.md#loop-mode) for the full behavior.

## `thinking_level`

Maps to a Gemini thinking budget:

| Value | Budget |
|---|---|
| `"off"` / `"none"` | 0 |
| `"low"` | 1024 |
| `"medium"` | 8192 |
| `"high"` | 24576 |
| `"dynamic"` / `"auto"` | -1 (model decides) |

## Schema notes

- `[security.rule]` is a list of rules — repeat the table for each. Order doesn't matter; `priority` decides.
- `[aliases.NAME]` is a single rule per alias.
- Most fields are optional; defaults are documented in [docs/design.md](design.md).
