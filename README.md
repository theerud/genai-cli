# genai-cli

A single-binary CLI for Google's Gemini API: chat REPL, one-off prompts, file attachments, image / TTS / music generation, sessions, roles, aliases, client-side function tools.

Inspired by [aichat](https://github.com/sigoden/aichat) but Gemini-only and dependency-light. Release binary ≈6 MB.

## Install

Requires Rust 1.88+ (edition 2024, let-chains) and OpenSSL development headers on the host (for `native-tls`).

```bash
git clone <repo> genai-cli && cd genai-cli
cargo build --release -p genai-cli
# binary at target/release/genai
```

Or install system-wide:

```bash
cargo install --path crates/genai-cli
```

## First-run setup

```bash
genai init
```

The wizard asks for your Gemini API key (get one at <https://aistudio.google.com/apikey>), picks a default chat model, and optionally installs starter roles. Generated files land under `~/.config/genai/`.

You can also set the API key out-of-band — `GEMINI_API_KEY` env, `./.env`, or `~/.config/genai/.env`. See [docs/config.md](docs/config.md#api-key).

## Hello, model

```bash
genai "Explain monads in two sentences"           # one-off
genai -m gemini-2.5-pro "..."                     # specific model
genai -f screenshot.png "What's broken here?"     # with attachment
genai -s research "let's pick up where we left"   # named session
genai                                             # drop into the REPL
```

Image / audio:

```bash
genai -m gemini-2.5-flash-image -o cat.png "a watercolor cat"
genai -m gemini-2.5-flash-preview-tts -o hi.wav "Hello there."
genai -m lyria-3-clip-preview -o tune.mp3 "lofi piano"
```

Pipe-friendly: when stdout isn't a TTY, output is plain text — no ANSI, no streaming flushes.

## Where to go next

| | |
|---|---|
| Architecture, conventions, principles | [docs/design.md](docs/design.md) |
| Config reference (`config.toml`, env vars) | [docs/config.md](docs/config.md) |
| REPL — prompt, dot-commands, completion | [docs/repl.md](docs/repl.md) |
| Tools, function-call loop, security policy | [docs/tools.md](docs/tools.md) |
| Sessions, history, `.undo`/`.retry` | [docs/sessions.md](docs/sessions.md) |
| Models registry, aliases, `models sync` | [docs/models.md](docs/models.md) |
| Roadmap, parking lot | [docs/roadmap.md](docs/roadmap.md) |
| Recipes — example roles, tools, configs | [docs/recipes/](docs/recipes/) |

## License

MIT — see [LICENSE](LICENSE).
