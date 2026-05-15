# Tools

Two kinds, mixed freely in a role's `tools = [...]` list and the `.tools` REPL command:

- **Gemini server-side built-ins** — run inside Gemini's infrastructure; we just declare them on the request.
- **Local (client-side) tools** — invoked by Gemini via function calling, executed by `genai` on your machine.

When any local tool is active, the chat path switches from streaming to a non-streaming function-call loop. Streaming with tool calls is deferred (see [roadmap.md](roadmap.md)).

## Gemini server-side built-ins

| Name | Effect |
|---|---|
| `google_search` | Web search, grounded |
| `url_context` | Fetch + ground on URLs server-side |
| `code_execution` | Sandboxed Python |

These run in Google's infrastructure. Add them to a role:

```toml
# ~/.config/genai/roles/research.toml
model = "gemini-2.5-pro"
system_prompt = "Cite sources when relevant."
tools = ["google_search", "url_context"]
```

## Built-in local tools

| Name | Args | Notes |
|---|---|---|
| `read_file` | `path` | Up to 256 KB of text; tilde-expanded; symlink-resolved |
| `list_dir` | `path` | Up to 200 entries with name + type + size |
| `fetch_url` | `url` | http(s) GET, up to 1 MB |
| `exec` | `command` | `sh -c …`; **subject to confirmation** |
| `write_file` | `path`, `content`, `mode?` | Up to 10 MB UTF-8 text; `overwrite` (default) or `append`; **subject to confirmation** |
| `generate_media` | `kind`, `prompt`, `output_path?`, `model?`, `preview?`, `image?`, `speech?`, `music?` | One tool for image / speech / music generation; auto-path under `data_dir/generated/`; **subject to confirmation** |

Confirmable tools (`exec`, `write_file`, `generate_media`) prompt `[y/N/A]` per call. `A` trusts the tool for the rest of the REPL session — see `.trust` below.

### `generate_media` shape

```jsonc
{
  "kind":        "image" | "speech" | "music",
  "prompt":      "...",                  // for speech, this is the text to read
  "output_path": "/abs/or/~/path.ext",   // optional; auto-named when omitted
  "model":       "imagen-4...",          // optional; falls back to cfg defaults
  "preview":     true,                   // image only, TTY only; default true

  "image":  { "aspect": "16:9", "count": 2, "input_paths": ["ref.png"] },
  "speech": { "voice": "Kore" },
  "music":  { }
}
```

- `aspect` / `count` are Imagen-only; warn-and-drop for nano-banana models, same as the CLI.
- `input_paths` enables nano-banana edit/variation workflows.
- `preview` defaults to `true` so a one-off generation flashes on screen on Kitty/iTerm2-class terminals. Set `preview: false` in loop-mode roles for intermediate generations where the user only cares about the final asset. Silent no-op on terminals without inline-image support.
- Multi-speaker TTS (Gemini 2.5) and Lyria 3's image input / lyrics / tempo are not yet wired through this tool; they will land in a follow-up slice under the same tool name.

## User-defined tools

Drop a TOML file in `<config_dir>/tools/<name>.toml`. The filename stem is the tool name; it must not shadow a built-in.

```toml
description = "Show recent git commits as 'hash subject' lines."
command = ["git", "-C", "{{path}}", "log", "--oneline", "-n", "{{limit}}"]
timeout_secs = 10
confirmation = false             # or "always" / "never"

[args.path]
type = "string"                  # string | integer | number | boolean
description = "Path to the working tree."
required = true

[args.limit]
type = "integer"
default = 10
```

- Execution is **argv-only** (no `sh -c`). `command` is a `Vec<String>` with `{{name}}` placeholders substituted from validated args.
- Type coercion is lenient (e.g. `"20"` for an integer is accepted).
- `<config_dir>/tools/bin/` is prepended to `PATH` only when a user-defined tool runs — drop helper scripts alongside the `.toml` and reference them by basename.
- The registry is built once per process; edits require a restart.

See [recipes/user-tools.md](recipes/user-tools.md) for worked examples.

## Function-call loop

When any local tool is enabled, the chat turn runs as:

1. Send `generateContent` with `tools.functionDeclarations` for the local tools (plus any built-ins).
2. If the response contains `functionCall` parts, execute each locally and append a single `user` message holding all `functionResponse` parts. Repeat.
3. Stop when the model returns a text-only response, or bail at the iteration cap (default 8, overridable per role via `max_iterations`, per invocation via `--max-iter`).
4. Persist the full exchange (user → model+tool back-and-forth → final text) atomically in a single transaction with one `turn_id`.

`.undo` removes the whole exchange. `.retry` removes it then re-asks the user prompt that started it.

### Loop mode

A role can declare `mode = "loop"` (see [config.md](config.md#roles)) so a single user prompt can drive many tool-call iterations — research agents, scripted file edits, etc.

- The spinner shows `[N/MAX] thinking…` / `[N/MAX] running <tool>…` so you can see where the loop is.
- Only the user prompt and the **final** assistant text are kept in session history. Intermediate function calls / tool responses are stored with `loop_internal = 1` and excluded from `messages_to_contents`, so the next turn isn't polluted by working memory. `.undo` still removes the whole exchange.
- When the cap is reached interactively, you're asked:
  ```
  [loop] reached 8/8 iterations. continue? [c=+8 more / N=N more / Enter=stop]
  ```
  `c` grants another full budget; a number grants that many; empty stops with a trailer like `_[loop ended at 8/8 iterations]_`.
- Non-interactive runs stop cleanly at the cap (same trailer, no prompt).

## Policy

Every tool call passes through a single rule-based policy. Each rule names a tool (or set), optionally matches against one string-valued arg, and assigns a decision.

### Schema

```toml
[[security.rule]]
tool = "exec"                    # exact name, glob ("*", "read_*"), or a list
arg = "command"                  # optional; the arg to match
patterns = ["git diff*", "ls*"]  # glob: `*` is the only wildcard; anchored both ends
decision = "allow"               # "allow" | "deny" | "prompt"
priority = 100                   # higher wins; ties broken by config order
```

### Evaluation

1. Walk rules in descending `priority`.
2. First rule whose `tool` matches AND (if `arg`/`patterns` set) whose `patterns` match the named arg wins.
3. If no rule matches, a built-in floor refuses:
   - Sensitive paths: `~/.ssh/*`, `~/.aws/*`, `~/.gnupg/*`, `~/.netrc`, `<config_dir>/.env`.
   - Private networks: `localhost`, `::1`, RFC1918 (`10.*`, `172.16-31.*`, `192.168.*`), `169.254.*` (incl. cloud metadata at `169.254.169.254`), `0.*`, `127.*`.
4. What survives the floor falls through to the tool's own default — `exec` and confirmable user tools prompt; read-only built-ins run silently.

For tools that take path args (`read_file`, `list_dir`), the policy matches against the *canonicalized* path. Symlinks are resolved before the rule check, so `ln -s ~/.ssh /tmp/x` can't bypass a path-based deny.

### Glob semantics

- `*` matches any run of characters (including empty).
- No `?`, no character classes, no regex.
- Anchored at both ends. To match anywhere, surround with `*`.

Examples: `git*` matches `git diff` but not `magit`. `*push` matches `git push` but not `pushed`. `*sudo*` matches anywhere.

### Common patterns

See [recipes/policy.md](recipes/policy.md) for worked examples (safe-command allowlist, dangerous-command denylist, host-specific overrides, role-specific patterns).

## Audit log

Every tool call (allow / deny / prompt-denied / error / ok) is appended to `<data_dir>/tool-log.jsonl` as one JSON line:

```json
{"ts":"2026-05-14T10:00:00Z","tool":"exec","args":{"command":"ls"},"result":"ok","preview":"exit=0"}
```

Soft-capped at 5000 lines (configurable); trimmed in place when the file grows 10% past the cap.

View it:

```bash
genai audit tail            # last 20, formatted
genai audit tail -n 100     # last 100
genai audit tail --json     # raw JSONL (for jq / grep)
```

In the REPL: `.audit [N]`.

I/O failures while writing the log are logged via `tracing::warn` but never break tool execution.

## Trust state

When you answer `A` (always) to a confirmation prompt, the tool is trusted for the rest of the REPL session — subsequent calls skip the prompt.

```
> use exec to run uname
[tool] exec(uname) (exec)
[tool] run `exec(uname)`? [y/N/A] a
[tool] 'exec' trusted for this session
[tool/ok] exit=0
```

Inspect / revoke:

```
.trust            # or .trust list
.trust drop exec  # revoke a specific tool
.trust clear      # forget all
```

Trust resets when the REPL exits. One-off mode treats `A` and `y` as identical (single turn).

## Notes on safety

- Sandboxing (`exec` chroot/bubblewrap/landlock) is **not** implemented. The policy + confirmation flow is the protection.
- DNS-rebinding defenses on `fetch_url` are **not** implemented. The literal-host match against the URL is best-effort.
- Per-role permission profiles are **not** implemented — all rules are global. See [roadmap.md](roadmap.md).
