# Monocle CLI

[한국어](./docs/README.ko.md)

> Terminal utility to control and use [Monocle AI](https://monocle-ai.com). Log in once, then chat with models, integrate Claude Code, or call Monocle's OpenAI-compatible API from your own apps — all with the same authenticated session.

## Prerequisites

None. `monocle` is a single self-contained binary — no Node.js, no npm, no runtime to install.

## 🚀 Setup

Install with one command (downloads a prebuilt binary from GitHub Releases):

**macOS / Linux**

```bash
curl -fsSL https://raw.githubusercontent.com/warmblood-kr/monocle-cli/main/install.sh | sh
monocle login
```

**Windows (PowerShell)**

```powershell
irm https://raw.githubusercontent.com/warmblood-kr/monocle-cli/main/install.ps1 | iex
monocle login
```

A browser opens — sign in with your organization account. In headless/SSH
environments it falls back automatically to device-code login (or force it with
`monocle login --device-code`).

> Prefer to build from source? With a Rust toolchain installed:
> `cargo install --git https://github.com/warmblood-kr/monocle-cli` (or
> `git clone … && cargo build --release` → `target/release/monocle`).

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
| `monocle models` | List available models (with modality) |
| `monocle chat [--model <id>] [--system-prompt <text>] [--system-prompt-file <path>] [--max-tokens <n>]` | Chat with a model (REPL or stdin) |
| `monocle audio transcribe [file] [--model <id>] [--language <code>] [--response-format <fmt>]` | OpenAI-compatible STT (file or stdin) |
| `monocle audio transcribe-azure [file] [--locale <code>] [--diarization] [--profanity <mode>] [--channels <list>] [--definition <json>]` | Azure Fast transcription |
| `monocle audio speech [text] -o <path> [--model <id>] [--voice <name>] [--format <fmt>]` | OpenAI-compatible TTS (text arg or stdin) |
| `monocle audio speech-azure [ssml] -o <path> [--format <fmt>]` | Azure SSML TTS |
| `monocle claude [...args]` | Launch Claude Code through Monocle (args pass through) |
| `monocle setup` | Globally route plain `claude` through Monocle (opt-in) |
| `monocle unset` | Remove the global `claude` routing |
| `monocle agent [prompt] [--workdir <dir>] [--model <id>] [--max-steps <n>] [--session <name>]` | **Experimental.** Headless agent loop with tools (read/write/edit + shell) |
| `monocle acp` | **Experimental.** Run as an [ACP](https://agentclientprotocol.com) agent over stdio (for editors / desktop / Craft) |

## 💬 Chat with models

List what your tenant has:

```console
$ monocle models
MODEL ID                  NAME                  MODALITY  OWNER       CONTEXT
────────────────────────  ────────────────────  ────────  ──────────  ───────
claude-sonnet-4-6         Claude Sonnet 4.6     chat      anthropic   200k
claude-opus-4-7           Claude Opus 4.7       chat      anthropic   200k
gpt-4o                    GPT-4o                chat      openai      128k
gpt-4o-mini-transcribe    GPT-4o mini STT       stt       openai      -
gpt-4o-mini-tts           GPT-4o mini TTS       tts       openai      -

5 model(s) available.
```

Interactive REPL:

```console
$ monocle chat --model claude-sonnet-4-6
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
$ echo "Summarize OAuth 2.0 in one sentence." | monocle chat
Using model: claude-sonnet-4-6
Router: https://api.monocle-ai.com
OAuth 2.0 is an authorization framework that lets applications access a user's resources on another service without sharing the user's password.
```

With a system prompt from a file:

```bash
monocle chat --system-prompt-file ./persona.md --model claude-opus-4-7
```

> [!NOTE]
> `monocle model chat` / `monocle model list` still work but are deprecated and will be removed in a future release.

## 🔊 Audio (STT / TTS)

`monocle audio …` calls the audio endpoints directly so you can iterate on parameters and isolate API-level issues without going through a frontend. The OpenAI-compatible and Azure variants are separate subcommands because their parameter schemas don't overlap.

### Transcribe

OpenAI-compatible (`/v1/audio/transcriptions`):

```bash
monocle audio transcribe meeting.wav --model gpt-4o-mini-transcribe --language en
```

Pipe audio from another tool:

```bash
ffmpeg -i talk.m4a -f wav - | monocle audio transcribe --filename talk.wav --model gpt-4o-mini-transcribe
```

Azure Fast (`/v1/speechtotext/transcriptions:transcribe`) — for diarization and longer file uploads:

```bash
monocle audio transcribe-azure meeting.wav \
  --locale en-US --locale ko-KR \
  --diarization \
  --profanity Masked
```

Need an Azure parameter we haven't exposed yet? Use the escape hatch:

```bash
monocle audio transcribe-azure meeting.wav --definition '{"locales":["ja-JP"],"customSetting":true}'
```

### Speech

OpenAI-compatible (`/v1/audio/speech`):

```bash
monocle audio speech "Hello from Monocle" --voice nova --format mp3 -o hello.mp3
```

Pipe text in and audio out:

```bash
echo "the quick brown fox" | monocle audio speech --voice alloy > sample.mp3
```

Azure SSML (`/v1/azure/texttospeech/cognitiveservices/v1`) — body must be SSML (start with `<speak …>`), so it's safer to keep it in a file than to escape it on the shell:

```bash
cat > /tmp/jenny.ssml <<'EOF'
<speak version="1.0" xmlns="http://www.w3.org/2001/10/synthesis" xml:lang="en-US">
  <voice name="en-US-JennyNeural">Hello there.</voice>
</speak>
EOF

monocle audio speech-azure \
  --format audio-24khz-48kbitrate-mono-mp3 \
  -o jenny.mp3 \
  < /tmp/jenny.ssml
```

On failure each command prints the HTTP status and response body to stderr and exits non-zero, which makes it easy to spot bad parameters or backend errors.

## 🤖 Claude Code integration

```bash
monocle claude
```

Other terminals and IDE integrations running plain `claude` are unaffected. To globally route plain `claude` through Monocle, run `monocle setup` once; undo with `monocle unset`.

> [!NOTE]
> See **[Claude Code integration details](./docs/claude-code.md)** for `ANTHROPIC_API_KEY` handling, global setup, and troubleshooting.

## 🧪 Experimental: agent & ACP

> These are **experimental** and evolving. They require you to be logged in — every
> LLM call is routed through Monocle (your chosen model, via `monocle login`).

**`monocle agent`** runs a headless agent loop with file (read/write/edit) and shell
tools in a working directory. Give it a task as an argument, pipe it via stdin, or omit
it for an interactive REPL. Progress goes to stderr, the answer to stdout; `--session
<name>` persists/resumes a conversation.

The interactive REPL has full line editing — arrow keys (←/→ to move, ↑/↓ for history),
and Emacs bindings (Ctrl-A/E to jump to line start/end, Ctrl-K to kill, Ctrl-Y to yank).
Command history persists across sessions in `~/.monocle/agent_history`.

In the interactive REPL, lines starting with `/` are local management commands (handled
without calling the model, printed to stderr): `/help` lists them, `/config` shows the
session config (model, max-steps, workdir, session), `/status` adds your login status,
and `/exit` (or `/quit`, Ctrl-D) quits.

```bash
monocle agent "summarize the TODOs in this repo" --workdir .
```

> ⚠️ Tools are auto-approved (read/write/edit + shell). Run only in a directory you trust.

**`monocle acp`** runs Monocle as an **[Agent Client Protocol](https://agentclientprotocol.com)**
agent over stdio (JSON-RPC) — an editor, the Monocle desktop app, or Craft spawns it and
drives sessions. Tool permission is delegated to the client (`session/request_permission`),
tool calls stream as `ToolCall`/`ToolCallUpdate` updates, and a client may pick the model
per session via `_meta.monocle.model` on `session/new` (falls back to the default).
When the client advertises the matching capabilities, the agent routes **file reads/writes
through the client** (`fs/read_text_file`/`fs/write_text_file`, so unsaved editor buffers are
honored) and **runs the shell via the client's terminal** (`terminal/*`); otherwise it falls
back to local disk and a local subprocess.

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
