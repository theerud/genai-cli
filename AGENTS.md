# AGENTS.md

Quick navigation for LLM agents (or humans) joining this codebase.

- **What this is**: a single-binary Rust CLI for Google's Gemini API — REPL chat, one-off prompts, image/TTS/music generation, sessions, roles, aliases, client-side function tools. See [docs/design.md](docs/design.md).
- **Source layout**: everything under `crates/genai-cli/src/`. See [docs/design.md](docs/design.md#project-layout) for the module map.
- **How to learn**: start with [docs/design.md](docs/design.md) for architecture, then dive into the area relevant to your task — [tools](docs/tools.md), [REPL](docs/repl.md), [sessions](docs/sessions.md), [models](docs/models.md), [config](docs/config.md).
- **Conventions**: see [docs/design.md](docs/design.md#principles). TL;DR: Gemini-only, minimal deps, single binary, single SQLite DB, hand-rolled HTTP/API client.
- **What's next**: [docs/roadmap.md](docs/roadmap.md) lists deferred work and track-only notes.

When making changes:

- Code style: idiomatic Rust, edition 2024, let-chains used. Run `cargo clippy --all-targets`; we keep it clean.
- Tests: `cargo test`. Add a test next to non-trivial logic.
- Docs: keep [docs/](docs/) in sync. The `[security.rule]` schema, dot-commands, and config keys are the most-referenced surfaces.
- Commits: Conventional Commits, single-line subject for most changes, body when context isn't obvious from the diff.
