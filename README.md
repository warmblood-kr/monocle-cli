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

> **Prebuilt binaries:** macOS (Apple Silicon), Linux (x86-64 / arm64), and Windows (x64).
> Intel Macs are not shipped as a prebuilt binary — build from source (below).

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
| `monocle chat [--model <id>] [--system-prompt <text>] [--system-prompt-file <path>] [--max-tokens <n>] [--file <path\|url>]... [--responses] [--resume <id>]` | Chat with a model (REPL or stdin); attach files/images with `--file` (one-shot only); `--responses` uses jarvice's server-managed-thread API instead |
| `monocle chat list` | List existing jarvice chat threads (id/title/last-updated) — including threads created in jarvice's own web UI |
| `monocle audio transcribe [file] [--model <id>] [--language <code>] [--response-format <fmt>]` | OpenAI-compatible STT (file or stdin) |
| `monocle audio transcribe-azure [file] [--locale <code>] [--diarization] [--profanity <mode>] [--channels <list>] [--definition <json>]` | Azure Fast transcription |
| `monocle audio speech [text] -o <path> [--model <id>] [--voice <name>] [--format <fmt>]` | OpenAI-compatible TTS (text arg or stdin) |
| `monocle audio speech-azure [ssml] -o <path> [--format <fmt>]` | Azure SSML TTS |
| `monocle claude [...args]` | Launch Claude Code through Monocle (args pass through) |
| `monocle setup` | Globally route plain `claude` through Monocle (opt-in) |
| `monocle unset` | Remove the global `claude` routing |
| `monocle upgrade [--check]` | Update monocle to the latest release (`--check` only reports current vs latest) |
| `monocle agent [prompt] [--workdir <dir>] [--model <id>] [--max-steps <n>] [--session <name>] [--auto-approve]` | **Experimental.** Headless agent loop with tools (read/write/edit + shell) |
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
▸ chat
❯ Hello
Hello! How can I help you today?

❯ /quit
Bye.
```

The interactive REPL has full line editing — arrow keys (←/→ to move, ↑/↓ for history),
and Emacs bindings (Ctrl-A/E to jump to line start/end, Ctrl-K to kill, Ctrl-Y to yank).
Tab completes `/help`/`/model`/`/diag`/`/quit`/`/exit` as a dropdown listing every
match (not cycling one at a time), with the best match also shown as a dim inline
hint as you type; a
multi-line paste is inserted as a single input (submitted only on Enter, not one turn
per line). Command history persists across sessions in `~/.monocle/chat_history` (kept
separate from `monocle agent`'s history). The REPL remembers the conversation — each
turn resends the full exchange so far, the same way `monocle agent` does — so
follow-up questions ("what about in Python?") work without repeating context.

Same as `monocle agent`'s REPL, `/model` shows the current model (or `/model <id>`
switches it for later turns), and `/model <TAB>` fuzzy-completes against the model ids
available to your account (fetched once at startup) — e.g. `/model cla<TAB>` narrows to
matching ids, and a bare `/model <TAB>` lists them all; if you're not logged in or the
list can't be fetched, completion just offers nothing (typing a full id still works).

`/diag` shows diagnostics for the **last** turn only — nothing is printed automatically
after a reply, it's opt-in on demand. Handy with a router alias (e.g. `monocle-auto`) to
see which concrete model actually served the response:

```console
❯ /diag
--- diag ---
Endpoint: https://api.monocle-ai.com/v1/chat/completions
Requested model: monocle-auto
Served model: claude-sonnet-4-6
Latency: 842ms
Tokens: 120 prompt + 45 completion = 165 total
--- end diag ---
```

`Served model` and `Tokens` are only shown when the backend reports them (always true for
`monocle chat`'s default path; `--responses`, below, reports neither today, so those lines
are simply omitted rather than printed as empty). Before the first turn, `/diag` just
prints a hint to send a message first.

`/help` re-prints the REPL's onboarding message (the same lines shown when the
session starts) — handy if you've scrolled past it.

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
> The reply **streams** to stdout as it is generated. `--max-tokens` is
> **omitted by default** so the model/router uses its own (model-appropriate)
> output limit — pass `--max-tokens <n>` only to cap it.

> [!NOTE]
> `monocle model chat` / `monocle model list` still work but are deprecated and will be removed in a future release.

### 📎 Attaching files/images

`monocle chat` can attach files (images in v1) to a one-shot (piped) message —
handy as an eval primitive for comparing how different providers/models handle
vision input:

```bash
echo "count the people in this image" | monocle chat --file photo.png --model gpt-4o
```

- `--file <PATH|URL>` is repeatable. A local path is read, MIME-sniffed by
  extension, and base64-encoded into a `data:<mime>;base64,...` URI; an
  `http://`/`https://` value is passed straight through as the remote image
  URL (no fetch).
- Only image types are wired to a vision request in v1 (`png`, `jpg`/`jpeg`,
  `gif`, `webp`). Anything else is a hard error: `unsupported type: <mime>`.
- You can also reference a file **inband**, inside the piped text itself,
  with an Org-mode-style `file:<path>` token — **not** a standard `file://`
  URI (no `//` required, and relative paths are fine):

  ```bash
  echo "compare file:./a.png and file:./b.png, which is sharper?" | monocle chat --model gpt-4o
  ```

  The `file:` token is stripped from the text sent to the model. Trailing
  sentence punctuation (`.,;:!?)'"`) right after the path is trimmed before
  resolving — a known v1 limitation if your path itself legitimately ends in
  one of those characters.
- Explicit `--file` flags resolve first (in the order given), then inband
  `file:` references in the order they appear in the text.
- An attachment-only message (no text) is valid.
- The `--file` flag is **one-shot only** — passing it while stdin is a
  terminal is an error asking you to pipe the instruction instead. Inband
  `file:<path>` tokens, however, also work when typed directly into the
  **interactive REPL**: a resolution failure (bad path, unsupported type)
  prints an error and returns you to the prompt rather than exiting the
  session.

This makes `monocle chat` a quick harness for an image-handling eval loop —
run the same image + instruction across models and diff both the answers and
the raw provider error messages:

```bash
for m in gpt-4o claude-sonnet-4-6 gemini-2-flash; do
  echo "=== $m ==="
  echo "count the people in this image" | monocle chat --file crowd.jpg --model "$m"
done
```

### 🧵 `--responses`: server-managed threads (experimental)

`monocle chat --responses` talks to jarvice's custom **Responses API**
(`/api/responses`) instead of the plain `/v1/chat/completions` path. It's not
OpenAI's Responses API — it's monocle's own endpoint, named for the similar
idea: the **server** persists the conversation thread, so the CLI sends only
the new turn instead of resending the whole exchange.

```bash
monocle chat --responses --model claude-sonnet-4-6
```

```console
Monocle Chat — Responses API (model: claude-sonnet-4-6)
jarvice: https://acme.monocle-ai.com
Type your message. Press Ctrl+D to exit.
---
▸ chat
❯ Hello
Hello! How can I help you today?
Thread: 3fa3b2c1-...

❯ /quit
Bye.
```

Continue a specific thread later (one-shot or REPL) with `--resume <id>`
(the id a previous run printed to stderr as `Thread: ...`):

```bash
echo "and in Python?" | monocle chat --responses --resume 3fa3b2c1-...
```

`/diag` also works in this REPL, but the Responses API doesn't echo back a served
model or token usage, so those lines are omitted — only `Endpoint`/`Requested
model`/`Latency` are shown.

List your existing threads (including ones started in jarvice's own web UI —
both share the same storage) with the `list` subcommand:

```bash
monocle chat list
```

Continuing into the interactive REPL with `--resume <id>` (not one-shot)
replays that thread's prior history to stderr before the prompt, so you can
pick up a conversation started elsewhere.

Notes:

- **jarvice-only** — this does not go through chat-proxy's router, so it
  needs jarvice's own tenant URL (derived the same way as the rest of this
  CLI, from your logged-in tenant domain). It's unrelated to `--model`
  routing and can't be redirected.
- **No streaming yet** — the reply is fetched non-streaming and printed once
  it's fully generated, unlike the plain chat path's live token stream.
- **`--system-prompt`/`--system-prompt-file`/`--max-tokens` are ignored** — the
  endpoint has no equivalent fields (a warning is printed if you pass them).
- **Known auth gap**: this endpoint currently rejects the CLI's access token
  with a 401 (`JWT missing required claim: email`) against real staging/prod
  tenants — the same open issue that blocks `monocle mcp` today. It's fine to
  use against a local/mock dev stack in the meantime.

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

## ⬆️ Upgrading

Update to the latest GitHub Release in place — same prebuilt binary as the
installer, swapped over the running executable:

```bash
monocle upgrade
```

Just check whether a newer version exists (no install):

```bash
monocle upgrade --check
```

Intel Macs have no prebuilt binary, so `upgrade` reports an error there — build
from source instead (see Setup).

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
Tab completes slash commands as a dropdown listing every match (not cycling one at a
time), with the best match also shown as a dim inline hint as you type; a multi-line
paste is inserted as a single input (submitted only on Enter). Command history
persists across sessions in `~/.monocle/agent_history`.

In the interactive REPL, lines starting with `/` are local management commands (handled
without calling the model, printed to stderr): `/help` lists them, `/config` shows the
session config (model, max-steps, workdir, session), `/status` adds your login status,
`/diag` shows diagnostics (served model, endpoint, latency, tokens, and step count) for
the last turn, `/model` shows the current model (or `/model <id>` switches it for later
turns), and `/exit` (or `/quit`, Ctrl-D) quits. `/model <TAB>` fuzzy-completes against the
model ids available to your account (fetched once at startup) — e.g. `/model cla<TAB>`
narrows to matching ids, and a bare `/model <TAB>` lists them all; if you're not logged in
or the list can't be fetched, completion just offers nothing (typing a full id still
works).

```bash
monocle agent "summarize the TODOs in this repo" --workdir .
```

> ⚠️ In an interactive session, each side-effecting tool call (write/edit + shell) asks
> for a `[y/N]` confirmation before it runs; anything but `y` denies it. Pass
> `--auto-approve` to skip the prompt (dangerous). Non-interactive runs (a prompt
> argument or piped stdin) have no TTY to prompt on and run tools unattended, so run
> those only in a directory you trust.

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

**"Error: error decoding response body" (or another network/streaming error)**
→ The request/response context (method, URL, underlying error) is appended to
`~/.monocle/cli.log` whenever this happens; a hint pointing there is printed alongside
the error. The file is otherwise never written or referenced — check it only when you
hit an error and need more detail than the one-line message.

For Claude Code and OpenAI SDK specific issues, see the linked guides above.

## Help

Contact your organization admin or open an issue in this repository.
