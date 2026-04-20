# Claude Code integration

[한국어](./claude-code.ko.md)

Monocle CLI lets you run [Claude Code](https://docs.anthropic.com/en/docs/claude-code) against your Monocle tenant instead of directly against the Anthropic API. Two modes are available: **invocation-scoped** (default, recommended) and **global opt-in**.

## Prerequisites

- `monocle login` has completed successfully (see the [main README](../README.md#-setup))
- **Claude Code** installed — [install here](https://docs.anthropic.com/en/docs/claude-code/getting-started)

## Invocation-scoped: `monocle claude`

```bash
monocle claude
```

Launches Claude Code with Monocle credentials scoped **only to this invocation** via a per-invocation `--settings` override. Your global Claude Code configuration is untouched — plain `claude` in other terminals and IDE integrations stays on whatever it was pointing at.

Extra arguments are passed through to Claude Code:

```bash
monocle claude --help
monocle claude -c      # resume most recent session
```

### `ANTHROPIC_API_KEY` handling

If `ANTHROPIC_API_KEY` is set in your environment, Claude Code prefers it over Monocle. `monocle claude` clears this variable in the child process environment automatically so Monocle credentials win — no manual `unset` needed.

## Global opt-in: `monocle setup`

To route plain `claude` (including IDE integrations and every other terminal) through Monocle globally:

```bash
monocle setup
```

This writes `apiKeyHelper: "monocle token"` into `~/.claude/settings.json`. Claude Code then calls `monocle token` to fetch a fresh access token per request.

Undo at any time:

```bash
monocle unset
```

Verify state:

```bash
monocle status   # look for "Claude Code: Configured"
```

## Troubleshooting

**Claude Code ignores Monocle**
→ An `ANTHROPIC_API_KEY` in your environment takes precedence. Use `monocle claude` (which clears it automatically) or `unset ANTHROPIC_API_KEY` in your shell.

**Plain `claude` doesn't use Monocle**
→ By design. `monocle claude` is invocation-scoped. Run `monocle setup` if you want plain `claude` (IDE integrations, other terminals) to also route through Monocle.

**Want to start fresh**
→ `monocle unset` removes global routing. Re-run `monocle setup` if needed.
