# Monocle CLI

[한국어](./docs/README.ko.md)

Monocle CLI is a utility to control and use [Monocle AI](https://monocle-ai.com) from your terminal. Log in once, then use that authenticated session for chat, model management, Claude Code integration, and calling Monocle's OpenAI-compatible API from your own apps.

## Prerequisites

- **Node.js** 18+ — check with `node -v`

## Setup (takes about 30 seconds)

**Step 1** — Install

```bash
npm install -g @warmblood/monocle-cli
```

**Step 2** — Log in

```bash
monocle login
```

A browser opens — sign in with your organization account. Monocle CLI runs the OAuth flow and stores your credentials at `~/.monocle/credentials.json` (file mode 0600), including a short-lived access token and a 30-day refresh token.

> **Tip:** To target a specific tenant: `monocle login --tenant your-org.monocle-ai.com`
>
> **Headless / SSH / CI?** `monocle login` auto-detects these environments and switches to the device code flow. Force it with `--device-code`.

## Check Status

```bash
monocle status
```

Shows your tenant, user, access/refresh token validity, and whether Claude Code is globally configured to route through Monocle. This command is read-only — it does not refresh tokens (refresh happens lazily when you run `monocle token` or use an integration).

## Commands

| Command | Description |
|---------|-------------|
| `monocle login [--tenant <domain>] [--env <env>] [--device-code]` | Sign in — browser by default, device code on headless/SSH/CI |
| `monocle status` | Show login, token validity, and Claude Code configuration status |
| `monocle token` | Print current access token (auto-refreshed when near expiry) |
| `monocle model list` | List models available on your tenant |
| `monocle model chat [--model <id>] [--system-prompt <text>] [--system-prompt-file <path>] [--max-tokens <n>]` | Chat with a model — interactive REPL or pipe from stdin |
| `monocle claude [...args]` | Launch Claude Code scoped **only to this invocation** (args pass through) |
| `monocle setup` | Globally route plain `claude` through Monocle (opt-in) |
| `monocle unset` | Remove the global `claude` routing |

## Chat with models (`monocle model`)

List what your tenant has:

```bash
monocle model list
```

Interactive REPL:

```bash
monocle model chat --model claude-sonnet-4-6
```

One-shot via stdin:

```bash
echo "Summarize OAuth 2.0 in one sentence." | monocle model chat
```

With a system prompt loaded from a file:

```bash
monocle model chat --system-prompt-file ./persona.md --model claude-opus-4-7
```

## Claude Code integration

Run Claude Code with Monocle credentials applied **only to this invocation** — no global config changes:

```bash
monocle claude
```

Plain `claude` in other terminals and IDE integrations is unaffected. If you want every `claude` invocation (including IDE) to route through Monocle, run `monocle setup` once; undo with `monocle unset`.

See **[Claude Code integration details](./docs/claude-code.md)** for `ANTHROPIC_API_KEY` handling, global setup, and troubleshooting.

## Using Monocle from your own app (OpenAI-compatible SDK)

Monocle exposes an OpenAI-compatible Chat Completions API, so any OpenAI client works with two pieces of config:

- **Base URL** — `router_url` from your credentials (typically `https://api.monocle-ai.com`)
- **API key** — output of `monocle token`

Minimal Python example:

```python
import subprocess
from openai import OpenAI

token = subprocess.check_output(["monocle", "token"], text=True).strip()
client = OpenAI(api_key=token, base_url="https://api.monocle-ai.com/v1")

resp = client.chat.completions.create(
    model="claude-sonnet-4-6",
    messages=[{"role": "user", "content": "Hello"}],
)
print(resp.choices[0].message.content)
```

See **[Using Monocle with the OpenAI SDK](./docs/openai-sdk.md)** for Node.js, `curl`, streaming, token-refresh patterns for long-running apps, and troubleshooting.

## Troubleshooting

**"Not logged in" error**
→ Run `monocle login` first.

**Token expired**
→ `monocle token` auto-refreshes when near expiry. If it's been more than 30 days since your last login (refresh token TTL), run `monocle login` again.

For Claude Code and OpenAI SDK specific issues, see the linked guides above.

## Help

Contact your organization admin or open an issue in this repository.
