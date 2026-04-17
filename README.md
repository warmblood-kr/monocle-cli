# Monocle CLI

[한국어](./docs/README.ko.md)

Monocle CLI connects [Claude Code](https://docs.anthropic.com/en/docs/claude-code) to [Monocle AI](https://monocle-ai.com).

Log in once and you're done — no API key management, no manual config file editing.

## Prerequisites

- **Node.js** 18+ — check with `node -v`
- **Claude Code** — [install here](https://docs.anthropic.com/en/docs/claude-code/getting-started) if you haven't already

## Setup (takes about 30 seconds)

**Step 1** — Install Monocle CLI

```bash
npm install -g @warmblood/monocle-cli
```

**Step 2** — Log in

```bash
monocle login
```

A browser window will open — sign in with your organization account.
Claude Code will be configured automatically after login.

That's it! Run `monocle claude` to get started.

> **Tip:** To specify a tenant explicitly, use `monocle login --tenant your-org.monocle-ai.com`
>
> **Tip:** If you have `ANTHROPIC_API_KEY` set in your environment, `monocle claude` will automatically clear it to avoid conflicts.
>
> **Headless / SSH / CI?** `monocle login` auto-detects these environments and switches to
> the device code flow — a URL and short code you enter on another machine. If auto-detection
> misses your environment, force it with `monocle login --device-code`.

## Check Status

```bash
monocle status
```

All green (**Valid** and **Configured**) means you're good to go!

## Commands

| Command | Description |
|---------|-------------|
| `monocle login [--tenant <domain>] [--env <env>] [--device-code]` | Sign in — browser by default, device code on headless/SSH/CI (auto-configures Claude Code) |
| `monocle setup` | Manually configure Claude Code (usually handled by login) |
| `monocle claude` | Launch Claude Code with conflicting env vars cleared |
| `monocle status` | Show login and configuration status |
| `monocle token` | Print current access token (used internally by Claude Code) |
| `monocle unset` | Remove Monocle configuration from Claude Code |

## Troubleshooting

**"Not logged in" error**
→ Run `monocle login` first.

**Token expired**
→ Tokens are refreshed automatically. If it's been more than 30 days since your last login, run `monocle login` again.

**Auto-setup failed**
→ Run `monocle setup` manually.

**Claude Code is ignoring Monocle**
→ An `ANTHROPIC_API_KEY` environment variable takes precedence over Monocle. Use `monocle claude` to launch Claude Code — it clears the conflict automatically.

**Want to start fresh?**
→ Run `monocle unset`, then `monocle setup`.

## Help

Contact your organization admin or open an issue in this repository.
