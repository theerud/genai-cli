# Research role

A role with Gemini's web search and URL grounding enabled.

```toml
# ~/.config/genai/roles/research.toml
model = "gemini-2.5-pro"
system_prompt = """
You are a research assistant. Use google_search for current events and
factual lookups. Cite the URLs you used inline. Prefer recent sources.
"""
tools = ["google_search", "url_context"]
```

```bash
genai -r research "what major space launches happened last week?"
genai -r research -s climate-news "any news on the X conference?"
```

## Variations

**With local URL fetching** (for sources Gemini's `url_context` can't reach, e.g. paywalls, internal docs):

```toml
model = "gemini-2.5-pro"
system_prompt = "Cite sources inline."
tools = ["google_search", "url_context", "fetch_url"]
```

Note: `fetch_url` is subject to the security policy. By default it refuses `localhost` and RFC1918 ranges. Add an allow rule if you want to grant it your internal wiki:

```toml
[[security.rule]]
tool = "fetch_url"
arg = "url"
patterns = ["https://wiki.internal.example.com/*"]
decision = "allow"
priority = 200
```

**Loop-mode with a report + hero image** (uses `generate_media` and `write_file`):

```toml
model = "gemini-2.5-pro"
mode = "loop"
max_iterations = 20
temperature = 0.3
system_prompt = """
You are a research agent. Search, fetch sources, cite inline, and produce
the deliverable the user asked for. When the user requests a report:
1. Gather material with google_search + fetch_url.
2. Generate a hero image with generate_media (kind="image", preview=true).
3. Write the final HTML/Markdown with write_file, embedding the image path.
4. Reply with a one-paragraph summary and the output path.
"""
tools = ["google_search", "url_context", "fetch_url", "write_file", "generate_media"]
```

```bash
genai -r research "history of analog synths, output as HTML at ~/reports/synths.html"
```

`generate_media` writes under `<data_dir>/generated/` when `output_path` is omitted; the path it returns is what the next `write_file` call should embed (`<img src="...">`). `preview = true` shows the image inline on a TTY; leave it `false` for intermediate generations you don't want to flash on screen.

## Notes

- `google_search` runs in Google's infrastructure; we just declare it on the request. Doesn't go through the function-call loop.
- Output stays streaming when the role uses only Gemini server-side tools. Streaming flips off only when a *local* tool is active.
- Citations appear inline in the model's output. Gemini's training is good at this with the `Cite sources` system-prompt hint.
