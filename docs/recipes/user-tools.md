# Writing a user-defined tool

Drop a TOML file in `<config_dir>/tools/<name>.toml`. The filename stem is the tool name; it must not shadow a built-in (`read_file`, `list_dir`, `fetch_url`, `exec`).

## Minimal example: git log

```toml
# ~/.config/genai/tools/git_log.toml
description = "Show recent git commits as 'hash subject' lines."
command = ["git", "-C", "{{path}}", "log", "--oneline", "-n", "{{limit}}"]
timeout_secs = 10

[args.path]
type = "string"
description = "Path to the working tree."
required = true

[args.limit]
type = "integer"
default = 10
```

Use it:

```toml
# in any role
tools = ["git_log"]
```

```
> .role <whatever-has-git-log>
> show me the last 5 commits in /home/me/myproject
```

## Helper scripts via `tools/bin/`

The directory `<config_dir>/tools/bin/` is prepended to `PATH` only when a user-defined tool runs. Drop helper scripts there and reference them by basename.

```bash
$ cat ~/.config/genai/tools/bin/say-hello
#!/usr/bin/env bash
echo "Hello, ${1:-world}!"
$ chmod +x ~/.config/genai/tools/bin/say-hello
```

```toml
# ~/.config/genai/tools/greet.toml
description = "Greet someone by name."
command = ["say-hello", "{{who}}"]

[args.who]
type = "string"
required = true
```

## Confirmation policy

Three forms accepted:

```toml
confirmation = true                # always prompt
confirmation = false               # never (default)
confirmation = "always"            # same as true; clearer intent
confirmation = "never"             # same as false
```

Future policies (`"outside-cwd"`, etc.) will plug in here without breaking back-compat.

## Argument types

| Type | TOML | Coerces from |
|---|---|---|
| `"string"` | a string | anything (model output stringified) |
| `"integer"` | a TOML int | TOML int, JSON number with integer value, JSON string parseable as i64 |
| `"number"` | a TOML float | TOML int/float, JSON string parseable as f64 |
| `"boolean"` | a TOML bool | bool, or one of `true`/`false`/`yes`/`no`/`y`/`n`/`1`/`0` |

`required = true` + missing → tool returns an error to the model (which usually reprompts itself).
`default = ...` + missing → uses the default.

## Substitution

`{{name}}` placeholders in `command` are replaced with the validated arg's value as a string. The substitution is plain string interpolation — no shell escaping needed because execution is argv-only.

```toml
command = ["echo", "{{n}} bottles of {{drink}}"]
```

## Subjecting your tool to policy

Once a user tool exists, the policy applies just like to built-ins. Match by tool name:

```toml
[[security.rule]]
tool = "deploy_staging"
decision = "always"
priority = 100
```

Or by arg pattern:

```toml
[[security.rule]]
tool = "git_log"
arg = "path"
patterns = ["~/work/*"]
decision = "allow"
priority = 100

[[security.rule]]
tool = "git_log"
arg = "path"
patterns = ["*"]
decision = "deny"
priority = 50    # falls through to deny anything outside ~/work
```

## What user tools don't do (yet)

- **Streaming output.** The model gets the full stdout/stderr at once, after the command exits or times out.
- **Stdin input.** Tools can't read from stdin — argv only.
- **Long-running daemons.** `timeout_secs` defaults to 30, capped at whatever you set.
- **Interactive prompts.** Don't write tools that need user keyboard input mid-run.
