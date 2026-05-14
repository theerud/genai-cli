# Policy recipes

Common patterns for `[[security.rule]]`. All examples go in `config.toml`. See [tools.md#policy](../tools.md#policy) for full semantics.

## Allow safe shell commands without prompts

```toml
[[security.rule]]
tool = "exec"
arg = "command"
patterns = [
    "git diff*", "git log*", "git status*", "git show*",
    "ls*", "cat*", "pwd", "uname*", "echo *",
    "rg *", "ag *", "grep *", "fd *", "find *",
]
decision = "allow"
priority = 100
```

## Block dangerous patterns even when a broader allow exists

```toml
[[security.rule]]
tool = "exec"
arg = "command"
patterns = [
    "sudo*", "rm -rf*", "* rm -rf *",
    "curl * | sh*", "wget * | sh*",
    ":(){:|:&};:",   # the classic fork bomb
]
decision = "deny"
priority = 200       # wins over the allow above
```

## Allow specific files inside an otherwise-denied directory

```toml
# Default deny credentials (this is also the built-in floor)
[[security.rule]]
tool = ["read_file", "list_dir"]
arg = "path"
patterns = ["*/.ssh/*"]
decision = "deny"
priority = 100

# Allow known_hosts specifically
[[security.rule]]
tool = "read_file"
arg = "path"
patterns = ["*/.ssh/known_hosts"]
decision = "allow"
priority = 200
```

## Trust your own LAN service

```toml
[[security.rule]]
tool = "fetch_url"
arg = "url"
patterns = ["http://192.168.1.50:8080/*"]
decision = "allow"
priority = 200       # higher than the built-in private-IPv4 deny
```

## Refuse cloud metadata even when called via a hostname

```toml
[[security.rule]]
tool = "fetch_url"
arg = "url"
patterns = ["*169.254.169.254*"]
decision = "deny"
priority = 1000      # very high; almost never overridable
```

(Note: this is already in the built-in floor. Adding it explicitly makes it auditable in `config.toml`.)

## Global default-deny: explicit allowlist mode

If you want NOTHING to run without an explicit allow rule:

```toml
[[security.rule]]
tool = "*"
decision = "deny"
priority = -100      # negative so any positive-priority allow wins

[[security.rule]]
tool = "exec"
arg = "command"
patterns = ["git status", "git log*", "ls*"]
decision = "allow"
priority = 100
```

Hostile, but rigorous.

## Tool-level allow (no arg matching)

If you want a tool to always run without prompt, regardless of args:

```toml
[[security.rule]]
tool = "read_file"
decision = "allow"
priority = 50        # below built-in floor; floor still wins on sensitive paths
```

Without `arg`, the rule matches any invocation of the named tool(s). Use this for tools you fully trust.

## How to verify a rule does what you think

Enable debug logging:

```bash
GENAI_LOG=genai::tools::policy=debug genai ...
```

Each evaluated tool call logs the matched rule (or `default`/`builtin:...`). Cross-reference with `genai audit tail` to see the actual decision history.

## Gotchas

- **Patterns are anchored.** `git` matches exactly `git`, not `git status`. Use `git*` or `*git*` if you want partial.
- **Order doesn't decide ties.** Priority does. If you have two rules at the same priority, *config-file order* breaks the tie — but don't rely on that; set distinct priorities.
- **Tool selector vs args.** `tool = "*"` with no `arg` matches every call regardless of args. Add `arg = "command"` and `patterns` to narrow.
- **Path canonicalization** runs before matching for `read_file` / `list_dir`. Don't write `~/.ssh` in a deny pattern thinking it'll match the literal string the model sent — write `*/.ssh/*` instead, which matches the resolved absolute path.
