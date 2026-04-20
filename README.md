# Monocle CLI

[한국어](./docs/README.ko.md)

> Terminal utility to control and use [Monocle AI](https://monocle-ai.com). Log in once, then chat with models, integrate Claude Code, or call Monocle's OpenAI-compatible API from your own apps — all with the same authenticated session.

## Prerequisites

- **Node.js** 18+ — check with `node -v`

## 🚀 Setup

```bash
npm install -g @warmblood/monocle-cli
monocle login
```

A browser opens — sign in with your organization account.

## ✅ Check status

```bash
monocle status
```

Shows your tenant, user, access/refresh token validity, and whether Claude Code is globally configured to route through Monocle. Read-only — it does not refresh tokens.

## 📖 Commands

| Command | Description |
|---------|-------------|
| `monocle login [--tenant <domain>] [--device-code]` | Sign in |
| `monocle status` | Show login, token, and Claude Code configuration status |
| `monocle token` | Print current access token (auto-refreshed when near expiry) |
| `monocle model list` | List available models |
| `monocle model chat [--model <id>] [--system-prompt <text>] [--system-prompt-file <path>] [--max-tokens <n>]` | Chat with a model (REPL or stdin) |
| `monocle claude [...args]` | Launch Claude Code through Monocle (args pass through) |
| `monocle setup` | Globally route plain `claude` through Monocle (opt-in) |
| `monocle unset` | Remove the global `claude` routing |

## 💬 Chat with models

List what your tenant has:

```console
$ monocle model list
MODEL ID              NAME                  OWNER       CONTEXT
────────────────────  ────────────────────  ──────────  ─────────
claude-sonnet-4-6     Claude Sonnet 4.6     anthropic   200k
claude-opus-4-7       Claude Opus 4.7       anthropic   200k
gpt-4o                GPT-4o                openai      128k

3 model(s) available.
```

Interactive REPL:

```console
$ monocle model chat --model claude-sonnet-4-6
Monocle Chat (model: claude-sonnet-4-6)
Router: https://api.monocle-ai.com
Type your message. Press Ctrl+D to exit.
---
> Hello
Hello! How can I help you today?

> /quit
Bye.
```

One-shot via stdin:

```console
$ echo "Summarize OAuth 2.0 in one sentence." | monocle model chat
Using model: claude-sonnet-4-6
Router: https://api.monocle-ai.com
OAuth 2.0 is an authorization framework that lets applications access a user's resources on another service without sharing the user's password.
```

With a system prompt from a file:

```bash
monocle model chat --system-prompt-file ./persona.md --model claude-opus-4-7
```

## 🤖 Claude Code integration

```bash
monocle claude
```

Other terminals and IDE integrations running plain `claude` are unaffected. To globally route plain `claude` through Monocle, run `monocle setup` once; undo with `monocle unset`.

> [!NOTE]
> See **[Claude Code integration details](./docs/claude-code.md)** for `ANTHROPIC_API_KEY` handling, global setup, and troubleshooting.

## 🔌 Using Monocle from your own app (OpenAI-compatible SDK)

Monocle exposes an OpenAI-compatible Chat Completions API, so any OpenAI client works with two env vars. Export them once:

```bash
export MONOCLE_API_KEY="$(monocle token)"
export MONOCLE_BASE_URL="$(jq -r .router_url ~/.monocle/credentials.json)/v1"
```

Then from Python:

```python
import os
from openai import OpenAI

client = OpenAI(
    api_key=os.environ["MONOCLE_API_KEY"],
    base_url=os.environ["MONOCLE_BASE_URL"],
)

resp = client.chat.completions.create(
    model="claude-sonnet-4-6",
    messages=[{"role": "user", "content": "Hello"}],
)
print(resp.choices[0].message.content)
```

> [!NOTE]
> See **[Using Monocle with the OpenAI SDK](./docs/openai-sdk.md)** for Node.js, `curl`, streaming, token-refresh patterns for long-running apps, and troubleshooting.

## 🆘 Troubleshooting

**"Not logged in" error**
→ Run `monocle login` first.

**Token expired**
→ `monocle token` auto-refreshes when near expiry. If it's been more than 30 days since your last login, run `monocle login` again.

For Claude Code and OpenAI SDK specific issues, see the linked guides above.

## Help

Contact your organization admin or open an issue in this repository.
