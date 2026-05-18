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
| `generate_media` | `kind`, `prompt`, `output_path?`, `preview?`, `image?`, `speech?`, `music?` | One tool for image / speech / music generation; auto-path under `data_dir/generated/`; **subject to confirmation** |

Confirmable tools (`exec`, `write_file`, `generate_media`) prompt `[y/N/A]` per call. `A` trusts the tool for the rest of the REPL session — see `.trust` below.

### `generate_media` shape

The schema is **rebuilt each turn** from the *effective* image model for the active role. Resolution order:

1. `role.media.image` (per-role override in `role.toml`)
2. `cfg.media.image` (global default in `config.toml`)
3. `cfg.model.image.default` (legacy, deprecated)
4. Hardcoded fallback (`imagen-4.0-generate-001`)

The LLM only sees parameters that actually apply to the resolved model, so it can't be tempted by a knob the backend doesn't accept. Switching roles mid-REPL changes the schema starting on the next turn.

```jsonc
{
  "kind":        "image" | "speech" | "music",
  "prompt":      "...",                  // for speech, this is the text to read
  "output_path": "/abs/or/~/path.ext",   // optional; auto-named when omitted
  "preview":     true,                   // image only, TTY only; default true

  // When the active image model is Imagen-style (id starts with `imagen`):
  "image":  { "aspect": "16:9", "count": 2 },

  // When the active image model is conversational (gemini-*-image, nano-banana):
  "image":  { "input_paths": ["ref.png"] },

  "speech": { "voice": "Kore" },
  "music":  { }
}
```

- The top-level `model` field is **not exposed** to the LLM. Image / TTS / music model is fixed by config; the LLM cannot override mid-call.
- For Imagen-style: `aspect` enum-constrained to `1:1 / 16:9 / 9:16 / 4:3 / 3:4`; `count` integer-bounded to `1-4`.
- For conversational: only `input_paths` is exposed; ratio / variant cues must stay in the prompt verbatim. The schema description tells the LLM exactly that.
- `preview` defaults to `true` so casual one-offs flash on screen on Kitty/iTerm2-class terminals. Set `preview: false` in loop-mode roles for intermediate generations. Silent no-op on terminals without inline-image support.
- Multi-speaker TTS (Gemini 2.5) and Lyria 3's image / lyrics / tempo inputs are deferred to a follow-up slice — same tool name, additive sub-object fields.

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

> **The built-in floor is not a hard security boundary.** User rules are evaluated *before* it, so a sufficiently-high-priority `allow` rule on a sensitive path or private host **does** override the floor. This is intentional — it lets you grant `fetch_url` access to a specific internal host, or `read_file` to a specific file under `~/.aws/`, without rewriting the policy from scratch. If you want the floor to be unconditional, don't add overriding rules; treat it as a default rather than a sandbox.

For tools that take path args (`read_file`, `list_dir`, `write_file`, `generate_media`), the policy matches against the *canonicalized* path. Symlinks are resolved before the rule check, so `ln -s ~/.ssh /tmp/x` can't bypass a path-based deny.

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

## Security model and trust

Be honest about what this tool's policy layer does and doesn't do.

### Confirmation warnings

Side-effecting tools (`exec`, `write_file`, `generate_media`, confirmable user tools) prompt `[y/N/A]` before running. The confirm prompt now also matches the tool's call summary against a per-tool list of glob patterns and prepends a `⚠` line listing any matches, so the user has a visible cue before answering.

Defaults catch the obvious traps:

| Tool | Default patterns (excerpt) |
|---|---|
| `exec` | `*~/.ssh*`, `*~/.aws*`, `*~/.gnupg*`, `*/.env*`, `*rm -rf /*`, `*curl*\|*sh*`, `*eval $(*`, `*sudo*`, `*chmod 777*` |
| `write_file` | `*~/.ssh*`, `*~/.aws*`, `*authorized_keys*` |

Override per tool in `config.toml`. Setting any list replaces the default for that tool (so you can quiet a noisy default by listing only the patterns you actually care about; setting `[]` disables warnings for that tool entirely).

```toml
[security.warn]
exec = [
  "*rm -rf*",
  "*git push --force*",
  "*~/.ssh*",
]
write_file = []                  # I'm careful, no warnings please
fetch_url = ["*169.254.169.254*"]  # warn on cloud metadata even when allowed
```

Patterns use the same `*`-glob semantics as `[[security.rule]]` and are matched against the tool's `describe_call` summary string — the same text shown next to `[tool]` in the announcement.

### `exec` is a policy wildcard

The `exec` tool runs `sh -c <command>` — a shell. Fine-grained policies on other tools (deny `write_file` under `~/.ssh/`, deny `fetch_url` to private hosts) do **not** constrain what `exec` can do. The same effect — exfiltrate a file, write to a sensitive path, hit an internal host — is reachable through `exec` regardless of how the other tools are configured.

If you enable `exec` in a role, you are granting that role effective shell access, modulo the per-call `[y/N/A]` confirmation. Mitigations that work:

- **Pair every role that enables `exec` with a narrowing `[[security.rule]]`** that limits which commands are allowed without prompting. Example:
  ```toml
  [[security.rule]]
  tool = "exec"
  arg = "command"
  patterns = ["git status*", "git diff*", "ls *"]
  decision = "allow"
  priority = 200
  ```
- **Read the full command in the confirm prompt before answering `y`.** It shows the whole shell line, so `rm -rf /tmp/x && echo bad >> ~/.ssh/authorized_keys` is visible — refuse it.
- **Don't enable `exec` in starter roles.** The shipped `research` role intentionally doesn't.

### `fetch_url` allow rules trust the host string, not the resolved IP

The built-in floor checks the literal host in the URL against private-IP and private-hostname patterns *before* sending the request. If you write an `allow` rule for a specific domain, however, the rule trusts the domain name — DNS rebinding to a private IP between checks and the request is **not** prevented.

Treat allow rules on `fetch_url` as "trust this domain owner." Don't allow domains you don't control.

### Other things the policy is not

- **No `exec` sandboxing.** No chroot, bubblewrap, landlock, seccomp.
- **Per-role permission profiles are not implemented** — all `[[security.rule]]` entries are global. The active role decides which tools are enabled; the policy decides what each tool can do.
- **TOCTOU is mitigated but not eliminated.** The policy evaluates against a canonicalized path; the runtime uses the same canonicalized args. A symlink swap between those two points is a tiny window but still possible without `openat2`-class atomicity.
- **`.env` parser is line-oriented.** Single-line `KEY=VALUE` only. Quoted multi-line values, `export` prefixes, and `${var}` expansion are not supported. Keep one value per line.

### When a loop turn fails

If a loop role's chat turn aborts mid-iteration, the whole exchange is discarded from session history so the next turn isn't corrupted by partial state. The forensic trail is in the audit log — every individual tool call (including the one that failed) is recorded with full args and outcome:

```bash
genai audit tail -n 30
# or in the REPL
.audit 30
```
