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
| `monocle profile list` / `profile show [name]` / `profile use <name>` | List, inspect, or choose profiles |
| `monocle handoff [chat-id] [--yes]` | Select one hosted Chat conversation and hand it to Claude Code |

Select a saved Monocle login with `--profile` or `MONOCLE_PROFILE`. A command-line value takes precedence over the environment variable, then `~/.monocle/config.json`'s default, then `default`:

```bash
monocle --profile work status
monocle login --tenant acme.monocle-ai.com --profile work --env prod
monocle model list --profile work
MONOCLE_PROFILE=work monocle model chat
monocle profile use work
monocle profile show work
```

`--profile` names a saved connection; its environment is `prod`, `stg`, or `local`. `login --env` chooses the OIDC environment for that login and updates the selected profile's environment. Without `--env`, login reuses the selected profile's saved environment. A login that would replace an existing profile's different environment is rejected; use another profile to keep both sessions, or pass `--force` to intentionally replace the selected profile. The `default` profile continues to use `~/.monocle/credentials.json`; named profiles use `~/.monocle/profiles/<name>/credentials.json`. Credentials are stored separately from `~/.monocle/config.json` and are written with private file permissions. Commands fail if the selected profile is not logged in instead of borrowing another profile's credentials.

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

`monocle handoff` lists saved hosted Monocle Chat conversations for the selected profile. Choose one conversation, review its title and message count, and confirm before opening Claude Code. To use it in a script, provide a chat ID and `--yes`. The CLI reads only the selected conversation and passes a temporary, private transcript file to Claude Code; it does not edit or save the server chat. Non-text attachments are marked as omitted from the transcript.

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
