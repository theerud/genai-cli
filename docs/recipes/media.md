# Media role

A flexible "helpful assistant + media generation" role that uses `generate_media` opportunistically in chat mode and gives the user a terse explicit-mode shorthand for direct media requests.

```toml
# ~/.config/genai/roles/media.toml
model = "gemini-3-flash-preview"
tools = ["generate_media"]
system_prompt = """
You are a helpful assistant with access to the generate_media tool.

When the user attaches files with -f, their absolute paths appear on a
`[attached: ...]` line at the top of the message. Treat that line as
metadata for tool calls only — never echo it back to the user, and don't
mention the paths in chat replies unless explicitly asked.

Operate in one of two modes based on the message body (the text AFTER any
[attached: ...] line).

EXPLICIT MODE — body starts with `[image]`, `[speech]`, or `[music]`:
- Everything after the closing bracket is the verbatim prompt for that kind.
- Call generate_media once. Pass the prompt unchanged — do not paraphrase
  or strip stylistic cues (aspect ratios, lighting, voice qualities,
  instrumentation, etc.).
- If files were attached and the kind supports input references, pass
  their paths via the relevant sub-object (e.g. image.input_images,
  music.input_images, or top-level prompt_file for long text).
- After the tool returns, reply with the saved path ONLY. One line, no prose.

CHAT MODE — anything else:
- Be a normal assistant. Answer questions, hold a conversation, reason
  through problems.
- Use generate_media opportunistically — when the user clearly asks for
  media, or when an image / sound clip would genuinely improve your
  answer (visualizing a concept, demonstrating a sound, illustrating a
  process). When it's a judgment call, offer first and wait for the user
  to confirm rather than generating unprompted.
- After a chat-mode generation, mention the saved path naturally in your reply.
"""

# Optional: per-role overrides for the media models. Falls back to
# global [media] in config.toml when absent.
# [media]
# image  = "gemini-2.5-flash-image"          # nano-banana, supports edit inputs
# speech = "gemini-2.5-flash-preview-tts"
# music  = "lyria-3-clip-preview"
```

## Usage examples

### Image generation

```bash
# Casual one-off
genai -r media "make me a watercolor of a cat on a windowsill"

# Explicit mode with verbatim prompt (terse reply with path only)
genai -r media "[image] a brown tabby with white legs, nose, and tail tip; portrait orientation; 4:3"

# Edit an attached image (nano-banana family)
genai -r media -f input.jpg "[image] turn this into an oil painting"

# Multiple references (still up to 10)
genai -r media -f ref1.png -f ref2.png "[image] blend the moods of these two"
```

For Imagen models the schema exposes `aspect` (`1:1`, `16:9`, `9:16`, `4:3`, `3:4`) and `count` (1-4). For conversational image models (`gemini-*-image`, nano-banana) those fields aren't in the schema at all — the LLM keeps orientation words in the prompt and calls again for variants.

### Speech — single voice

```bash
# Casual
genai -r media "say 'welcome aboard' in a warm voice"

# Explicit, terse
genai -r media "[speech] welcome aboard, traveller. The night sails are catching the breeze."

# Pick a specific voice
genai -r media "[speech] read this calmly in Sulafat: ..."

# Long transcript via attachment (uses prompt_file under the hood)
genai -r media -f audiobook-chapter.txt "[speech] narrate this with Charon"
```

Inspect available voices:

```bash
genai voices list                       # all 30
genai voices list -g female -s warm     # filter by gender / style
```

In the REPL: `.voices [filter]` (filter is `m`/`f` or a style substring).

### Speech — two-speaker dialog

The transcript needs `Name:` line prefixes that match each speaker's `name`:

```bash
cat > /tmp/dialog.txt <<EOF
Alice: Hey Bob, did you watch the launch?
Bob: Yeah, that landing was clean.
Alice: They're getting fast at this.
Bob: Routine, almost.
EOF

genai -r media -f /tmp/dialog.txt \
  "[speech] read this dialog. Alice with Kore, Bob with Charon."
```

The tool rejects (with a clear error the LLM can recover from):

- Same voice for both speakers.
- Same name for both speakers.
- Anything other than exactly 2 speakers.
- A speaker name that doesn't appear as a `Name:` prefix in the transcript.

### Music

```bash
# Short ambient clip (Lyria 3 Clip — always ~30s)
genai -r media "[music] downtempo ambient, sparse piano, slowly building strings"

# Inline lyrics + structure
genai -r media "[music] indie folk, female vocals.
[Verse 1]
The river runs through valleys deep
Where memories of dusk still sleep

[Chorus]
Carry me home, carry me home
On the wind that knows where I roam"

# Long lyric sheet via prompt_file
genai -r media -f song-lyrics.txt "[music] post-rock instrumental ending in distortion"

# Mood-image-driven (Lyria multimodal)
genai -r media -f sunset.jpg \
  "[music] ambient electronic, slow build, ~90 seconds, in the mood of this image"
```

For Lyria 3 Pro (longer / studio-grade): `genai -r media -m lyria-3-pro-preview ...` or set `[media].music = "lyria-3-pro-preview"` in the role. Pro additionally accepts:

- `response_format`: `mp3` (default) or `wav`. Clip ignores wav.
- Timestamp tags like `[0:00-0:10]` inside the prompt for event timing.
- Longer durations steered by prompt phrasing (`"a 2-minute piece..."`).

### Long inputs — `prompt_file`

Any kind can substitute `prompt_file` for `prompt` when the content is long enough that round-tripping it through the LLM's output would be wasteful or risk truncation. The LLM should pick up the attached path from the `[attached: ...]` preamble:

```bash
genai -r media -f long-monologue.md "[speech] narrate this in Iapetus"
```

Capped at 1 MB. Sensitive-path floor applies (`~/.ssh/`, `~/.aws/`, etc. — the LLM can't aim it at a key file).

## Policy and trust

`generate_media` requires confirmation per call by default. For a workflow where you trust the model to generate freely, answer `A` once or add a `[[security.rule]]`:

```toml
[[security.rule]]
tool = "generate_media"
decision = "allow"
priority = 200
```

To restrict output paths to a specific directory:

```toml
[[security.rule]]
tool = "generate_media"
arg = "output_path"
patterns = ["~/Pictures/genai/*", "*/generated/*"]
decision = "allow"
priority = 200
```

Anything that doesn't match still flows through the confirm prompt, so you can scope blanket-allow narrowly.

## Notes

- The role-level `[media]` table in `media.toml` overrides the global `[media]` defaults in `config.toml`. Use it to pin specific models per role (e.g. an "imagen role" that always uses Imagen 4 Ultra; a "podcast role" that always uses Lyria 3 Pro).
- `preview` defaults to `true` on TTY for image gen, so casual one-offs flash on screen. Loop-mode media roles that don't want intermediate previews should instruct the model to set `preview: false` for working images and `true` only for the final asset.
- For all the structured controls the LLM sees (aspect ratios, counts, voices, etc.), inspect what's currently advertised in the schema with `GENAI_LOG=debug genai -r media "..." 2>&1 | head -200` and look at the `tools` payload sent to Gemini.
