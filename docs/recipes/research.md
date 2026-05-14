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

## Notes

- `google_search` runs in Google's infrastructure; we just declare it on the request. Doesn't go through the function-call loop.
- Output stays streaming when the role uses only Gemini server-side tools. Streaming flips off only when a *local* tool is active.
- Citations appear inline in the model's output. Gemini's training is good at this with the `Cite sources` system-prompt hint.
