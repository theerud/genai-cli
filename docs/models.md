# Models registry

The CLI ships with a curated list of known Gemini / Imagen / Lyria / TTS / embedding models in `crates/genai-cli/src/models/data.toml` (compiled into the binary). Each entry carries:

- `id`, `display_name`, `family`
- `capabilities` (curated labels: `chat`, `vision_in`, `audio_in`, `thinking`, `function_calling`, `tool_google_search`, `tool_url_context`, `tool_code_execution`, `image_out`, `tts`, `music_out`, `embed`)
- `context_window`, `max_output_tokens`
- `input_price_per_1m`, `output_price_per_1m` (USD; hand-curated)
- `supports_thinking`, `thinking_levels`
- `status` (`stable`, `preview`, `deprecated`)

`models list` shows the resolved registry grouped by capability:

```
genai models list
```

## Synced overlay

To pick up new preview models without rebuilding the binary:

```bash
genai models sync               # writes <data_dir>/models.toml
genai models sync --dry-run     # just print the diff
```

The sync fetches `models.list` from the Gemini API and writes a **full snapshot** to `<data_dir>/models.toml`. For ids the bundled list knows about, curated fields (pricing, capability labels, thinking levels) are merged in. New ids the API knows about get sync-guessed family / capabilities / status from their name patterns.

Bundled ids that disappear upstream stay served from the bundled list — they're not removed from the overlay so deprecated-upstream models don't vanish until you bump the bundled file.

The overlay is fully managed by the sync command. Hand-edits get clobbered on the next run. To add custom model ids the API doesn't list, **use an alias** — aliases pass any model id through to the API (unknown ids surface a warning, never a hard block).

## Aliases

Named bundles of `(model, params)` usable anywhere a model id is accepted:

```toml
[aliases.pro-high]
model = "gemini-2.5-pro"
thinking_level = "high"

[aliases.fast]
model = "gemini-2.5-flash-lite"
temperature = 0.3
```

```bash
genai -m pro-high "..."
# in the REPL:
> .model fast
```

Resolution: alias lookup first, then raw model id against the registry. Unknown ids emit a warning with Levenshtein-suggest matches but don't block.

## Thinking levels

For Gemini models that support a thinking budget, the `thinking_level` field maps to:

| Value | Budget |
|---|---|
| `"off"` / `"none"` | 0 |
| `"low"` | 1024 |
| `"medium"` | 8192 |
| `"high"` | 24576 |
| `"dynamic"` / `"auto"` | -1 (model decides) |

Set per-role, per-alias, or via `.set thinking <level>` in the REPL.

## Pricing freshness

Pricing is hand-curated against [Google's pricing page](https://ai.google.dev/pricing). The Gemini API does not return pricing in `models.list`, so the sync command doesn't refresh it. Treat costs reported by `.info` as estimates — review pricing in `data.toml` when Google announces changes.

External feeds like [simonw/llm-prices](https://www.llm-prices.com/) cover Gemini chat models well but miss Imagen / Lyria / TTS / embedding. An automated drift-detection command was scoped but deferred (see [roadmap.md](roadmap.md)).

## Adding a new bundled model

For maintainers:

1. Run `genai models sync --dry-run` to see what the API knows that the bundle doesn't.
2. Pick an entry worth promoting.
3. Add a `[[models]]` block to `crates/genai-cli/src/models/data.toml` with curated `capabilities`, `pricing`, `thinking_levels`, etc.
4. Run tests; the bundled file is parsed at process start.

For local-only overrides without rebuilding: edit `<data_dir>/models.toml` (will be overwritten by the next `models sync`).
