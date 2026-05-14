# Coding role

`genai init` offers this as a starter; here it is for reference.

```toml
# ~/.config/genai/roles/coding.toml
model = "gemini-2.5-pro"
system_prompt = """
You are a senior software engineer. Be precise. Answer with code where
it helps. Skip pleasantries; assume the reader is fluent.
"""
temperature = 0.4
thinking_level = "high"
```

```bash
genai -r coding "explain Pin<&mut T>"
genai -r coding -s rust-notes              # role + named session
genai -r coding -f buggy.rs "what's wrong?"
```

## Variations

**With function tools** for reading your codebase:

```toml
model = "gemini-2.5-pro"
system_prompt = "Senior engineer. Read the user's code before answering."
tools = ["read_file", "list_dir"]
```

**With code execution** (sandboxed in Gemini's infra, no local risk):

```toml
model = "gemini-2.5-pro"
tools = ["code_execution"]
```

## Tips

- `thinking_level = "high"` matters more for actual reasoning tasks than for simple "what does X do." Use `"medium"` or `"low"` for cheaper answers when the task isn't hard.
- `temperature = 0.4` tradess off some creativity for consistency. Bump up for brainstorming, down for "rewrite this function."
- For repeated edits to the same file, attach with `.file` once per REPL session — the model sees it as long as it's in history.
