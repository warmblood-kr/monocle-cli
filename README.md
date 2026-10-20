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
| `monocle login [--tenant <domain>] [--env <prod\|stg\|local>] [--device-code]` | Sign in to the selected profile/environment slot |
| `monocle --profile <name> [--env <env>] <command>` | Select a saved profile and, when specified, its separate environment credential slot |
| `monocle profile list` / `show [name]` / `use <name>` | List connections, inspect profile/environment, or set the default profile |
| `monocle status` | Show login, token, and Claude Code configuration status |
| `monocle token` | Print current access token (auto-refreshed when near expiry) |
| `monocle chat [--model <id>] [--file <path\|url>]... [--resume <id>] [--tool-ids <id,...>] [--verify-tool-firing[=<tool>]] [--show-tools]` | Chat with Monocle hosted Chat (REPL or stdin): the server owns the thread, so it shows up in the web UI; attach files/images with `--file` (one-shot only); `--resume <id>` continues a thread; `--tool-ids <id,...>` activates MCP/connected-app servers for the turn; `--verify-tool-firing[=<tool>]` (one-shot) asserts a server-executed tool actually ran — name a tool to require that one — as an exit code (`0` ran / `1` did not / `2` cannot determine); `--show-tools` prints the tools the server ran this turn (`Tools: a, b` / `none` / `unknown`) to stderr. For the direct router (streaming, system prompt, token stats) use `monocle router chat` |
| `monocle chat list` | List existing jarvice chat threads (id/title/last-updated) — including threads created in jarvice's own web UI |
| `monocle chat content <chat-id> [--format text\|json] [--show-tools]` | Print one hosted Chat thread's active message path to stdout (text only, attachments omitted, control characters stripped, untrusted content, 1 MiB cap). `--show-tools` replaces each raw tool-call block with a one-line summary (tool name, sub-agent `model_id`, `report_to_parent`); with `--format json` it adds a structured `tool_calls` list per message |
| `monocle knowledge create <name>` | Create an empty knowledge collection (name must be new); prints its id |
| `monocle knowledge upload <collection> <file-or-dir>...` | Upload and index pdf/docx/md/txt files into a collection (a directory contributes its top-level supported files). A failed file is reported and the rest continue (exit `1`); files already in the collection (same name) are skipped, so re-runs are safe. Prints `file-id<TAB>name` per stored file |
| `monocle knowledge list [<collection>]` | List collections (id, name, file count), or one collection's files |
| `monocle knowledge update <collection> [--name N] [--description D]` | Rename and/or re-describe a collection; sharing (access control) is left as is |
| `monocle knowledge remove <collection> <file>` | Detach one file (name or id) and keep the collection; the server also deletes the file itself unless another collection or chat still uses it |
| `monocle knowledge replace <collection> <file>` | Replace the same-named file with a new local version (new one is attached before the old one is removed). Prints `file-id<TAB>name` |
| `monocle knowledge reprocess <collection> <file> [--force]` | Re-extract the file's text on the server and re-index it in the collection |
| `monocle knowledge content <collection> <file>` | Print the text the server extracted from the file (to check parsing before testing search) |
| `monocle knowledge ask <collection> <question> [--model M] [--json]` | Ask one question with the collection attached (one model turn, not saved to your threads). Answer on stdout; the files it was grounded in on stderr (or `{"answer","sources"}` with `--json`). jarvice has no retrieval endpoint, so this is the only way to see whether stored content is used |
| `monocle knowledge delete <collection>` | Delete a collection **and its files** (kept if any file can't be deleted, so you can retry) |
| `monocle mcp ls [--json]` | List your MCP servers (name, enabled, auth state `none`/`connected`/`needed`/`failed`, tool count); `--json` prints the server body unchanged |
| `monocle mcp show <server> [--json]` | Show one server (name or id) and its tools with each tool's enabled state |
| `monocle mcp attention [--json]` | Print how many servers need re-authentication |
| `monocle mcp enable <server>` / `disable <server>` | Turn a server on or off for your account |
| `monocle mcp tool <server> <tool> enable\|disable` | Switch one tool of an enabled server on or off (only flips the setting; never runs the tool) |
| `monocle mcp thread get <chat-id> [--json]` | Print the MCP server ids set on one chat. **A display mirror only** (jarvice's `ChatMcpEnabled` table): it does not prove the tool is called. To confirm firing, run a chat turn with `monocle chat --tool-ids mcp:SERVER --verify-tool-firing` |
| `monocle mcp thread set <chat-id> (<server>... \| --servers a,b \| --none) [--json]` | **Replace the whole** MCP server set of a chat and print the resulting set. Ids are catalog ids from `mcp ls`; jarvice does not reject unknown ids. Giving no servers is an error: clearing needs an explicit `--none`. Same display-mirror caveat as `get` (it does not guarantee the tool is called; verify with `chat --tool-ids mcp:SERVER --verify-tool-firing`) |
| `monocle workspace-model list` | List workspace (custom) models: id, name, base model, active |
| `monocle workspace-model show <id>` | Print one model as JSON |
| `monocle workspace-model create <id> --base <base-model> [--system <prompt>] [--tool <id>]... [--knowledge <collection>]... [--name N] [--description D] [--file m.json]` | Create a private, active model; prints its id. Flags override fields from `--file` (a JSON `ModelForm` fragment). Access: `--access private\|public`, or `--read-group/--write-group/--read-user/--write-user ID` (repeatable) to share |
| `monocle workspace-model update <id> [same flags]` | Change only the given fields (`--tool`/`--knowledge` replace the lists; `--clear-tools`/`--clear-knowledge` detach all; `--file` `meta`/`params` merge key-wise onto the stored ones) |
| `monocle workspace-model toggle <id>` / `delete <id>` | Flip active/inactive; delete |
| `monocle craft [--lane openworker\|sdk] session list\|show <id>\|history <id>\|files <id>\|download <id> <path>\|delete <id>\|delegate <id>`, `craft workspace list\|open <path> [--create]`, `craft agent list`, `craft turn TEXT` | Run a Craft turn (either lane), inspect and delete Craft sessions, list/open workspaces, list agents, attach delegation; see [Craft sessions](#-craft-sessions) |
| `monocle router models` | List available models from the router (with modality) |
| `monocle router chat [--model <id>] [--system-prompt <text>] [--system-prompt-file <path>] [--max-tokens <n>] [--file <path\|url>]...` | Chat via the router's plain `/v1/chat/completions` (REPL or stdin); no Responses-API options |
| `monocle router audio transcribe [file] [--model <id>] [--language <code>] [--response-format <fmt>]` | OpenAI-compatible STT (file or stdin) |
| `monocle router audio transcribe-azure [file] [--locale <code>] [--diarization] [--profanity <mode>] [--channels <list>] [--definition <json>]` | Azure Fast transcription |
| `monocle router audio speak [text] -o <path> [--model <id>] [--voice <name>] [--format <fmt>]` | OpenAI-compatible TTS (text arg or stdin) |
| `monocle router audio speak-azure [ssml] -o <path> [--format <fmt>]` | Azure SSML TTS |
| `monocle router image generate [prompt] --model <id> -o <path> [--size <WxH>] [--n <n>] [--quality <q>]` | OpenAI-compatible image generation (prompt arg or stdin) |
| `monocle router image edit <image> [prompt] --model <id> -o <path> [--mask <path>] [--size <WxH>] [--n <n>] [--quality <q>]` | OpenAI-compatible image edit (gpt-image models only; prompt arg or stdin) |
| `monocle claude [...args]` | Launch Claude Code through Monocle (args pass through) |
| `monocle setup` | Globally route plain `claude` through Monocle (opt-in) |
| `monocle unset` | Remove the global `claude` routing |
| `monocle upgrade [--check]` | Update monocle to the latest release (`--check` only reports current vs latest) |
| `monocle agent [prompt] [--workdir <dir>] [--model <id>] [--max-steps <n>] [--session <name>] [--auto-approve]` | **Experimental.** Headless agent loop with tools (read/write/edit + shell) |
| `monocle acp` | **Experimental.** Run as an [ACP](https://agentclientprotocol.com) agent over stdio (for editors / desktop / Craft) |

Profile selection is consistent across authenticated commands. `--profile` takes precedence over `MONOCLE_PROFILE`, then the saved default, then `default`. Each profile remembers its environment; `--env` selects a separate credential slot without replacing another environment's session. The default profile continues to use the existing `~/.monocle/credentials.json` when it matches the saved or inferred environment.

Upgrading from a release without profiles needs no re-login: on first run the existing session becomes the `default` profile, `~/.monocle/config.json` is backed up once to `~/.monocle/config.json.pre-profiles`, and an existing `apiKeyHelper` of `monocle token` keeps working. Downgrading is not supported; to go back, restore that backup.

Known limits: the first-run migration rewrites `config.json` without a lock, so a config change made by another `monocle` process at that same moment can be lost (first run only). If the default profile's saved environment has no credentials of its own but the existing session is for another environment, the default profile uses that session — this also applies to plain `monocle login`, which then signs in to the existing session's environment; pass `--env` to choose explicitly.

### Deprecated aliases

These root forms still work and print a one-line warning on stderr (stdout is unchanged). They will be removed in 2.0.

| Deprecated | Use instead |
|------------|-------------|
| `monocle models` | `monocle router models` |
| `monocle audio transcribe[-azure]` | `monocle router audio transcribe[-azure]` |
| `monocle audio speech[-azure]` | `monocle router audio speak[-azure]` |
| `monocle image <generate\|edit>` | `monocle router image <generate\|edit>` |
| `monocle model chat` | `monocle router chat` |

`monocle model list` has been removed; use `monocle router models`.

### Upgrading from 1.7 to 1.8

| Change | What to do |
|--------|------------|
| `echo q \| monocle chat` now creates a hosted thread (web UI, no streaming/token stats) | For the old direct-router behaviour use `monocle router chat`. `--system-prompt*`/`--max-tokens` on `chat` are ignored with a warning; `--responses` is a no-op |
| `chat --model X` is no longer validated against the router model list | Nothing — hosted Chat decides which models it accepts |

### Upgrading from 1.6 to 1.7

| Change | What to do |
|--------|------------|
| Profiles arrive; your existing login becomes the `default` profile on first run (config backed up to `~/.monocle/config.json.pre-profiles`) | Nothing — no re-login, `monocle token` / `apiKeyHelper` keep working |
| `models`, `audio`, `image`, `model chat` move under `monocle router` | Old forms still work with a stderr warning; switch to the new form before 2.0 |
| `audio speech[-azure]` is renamed | Use `monocle router audio speak[-azure]` |
| `monocle model list` is removed | Use `monocle router models` |
| `monocle model chat --responses` is now an error (exit 2) | Use `monocle chat` |
| New `monocle chat content <id> [--format text\|json]`; chat ids are validated strictly and control characters are stripped from output | — |
| `zip` dependency upgraded (RUSTSEC-2026-0367/0368/0369, Windows `upgrade` only) | — |

## 💬 Chat with models

List what your tenant has:

```console
$ monocle router models
MODEL ID                  NAME                  MODALITY  OWNER       CONTEXT
────────────────────────  ────────────────────  ────────  ──────────  ───────
claude-sonnet-4-6         Claude Sonnet 4.6     chat      anthropic   200k
claude-opus-4-7           Claude Opus 4.7       chat      anthropic   200k
gpt-4o                    GPT-4o                chat      openai      128k
gpt-4o-mini-transcribe    GPT-4o mini STT       stt       openai      -
gpt-4o-mini-tts           GPT-4o mini TTS       tts       openai      -

5 model(s) available.
```

The examples below use `monocle router chat`, which talks to the router's
plain `/v1/chat/completions` directly (live streaming, system prompts, token
stats). Plain `monocle chat` is hosted Chat — see
[Hosted Chat](#-hosted-chat-server-managed-threads) below.

Interactive REPL:

```console
$ monocle router chat --model claude-sonnet-4-6
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
Time to first byte: 312ms
Latency (total): 842ms
Tokens: 120 prompt + 45 completion = 165 total
--- end diag ---
```

`Time to first byte` is how long the response took to *start* streaming; `Latency (total)`
is how long the full reply took to finish. `Served model` and `Tokens` are only shown when
the backend reports them (always true for `monocle router chat`; hosted `monocle chat`,
below, reports neither today, so those lines are simply omitted rather than printed as
empty); hosted `monocle chat` also makes a single blocking call with no incremental deltas, so
`Time to first byte` has nothing to measure there and is omitted too. Before the first
turn, `/diag` just prints a hint to send a message first.

`/diag on` makes diagnostics print automatically after every reply, instead of only
on demand — handy for keeping an eye on token usage as you go. `/diag off` turns it
back off. The setting is saved to `~/.monocle/config.json` and persists across
sessions until toggled again.

`/help` re-prints the REPL's onboarding message (the same lines shown when the
session starts) — handy if you've scrolled past it.

One-shot via stdin:

```console
$ echo "Summarize OAuth 2.0 in one sentence." | monocle router chat
Using model: claude-sonnet-4-6
Router: https://api.monocle-ai.com
OAuth 2.0 is an authorization framework that lets applications access a user's resources on another service without sharing the user's password.
```

With a system prompt from a file:

```bash
monocle router chat --system-prompt-file ./persona.md --model claude-opus-4-7
```

> [!NOTE]
> The reply **streams** to stdout as it is generated. `--max-tokens` is
> **omitted by default** so the model/router uses its own (model-appropriate)
> output limit — pass `--max-tokens <n>` only to cap it.

> [!NOTE]
> `monocle model chat` is a deprecated alias for `monocle router chat` (see [Deprecated aliases](#deprecated-aliases)); `monocle model list` has been removed.

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

### 📚 Knowledge collections

Create and fill the same knowledge collections the web UI's Knowledge page shows, e.g. to set up repeatable retrieval test fixtures:

```bash
monocle knowledge create my-fixtures                 # prints the new collection id
monocle knowledge upload my-fixtures ./docs a.pdf    # dirs and/or files; pdf, docx, md, txt
monocle knowledge list                               # id, name, file count
monocle knowledge list my-fixtures                   # the collection's files
monocle knowledge delete my-fixtures                 # collection and its files
```

### 🧩 Workspace (custom) models

Create, use and clean up the same models as the web UI's Workspace > Models page (needs the `workspace.models` permission; without it jarvice answers 401 and the CLI says so). Named `workspace-model` because `monocle models`/`monocle model` are deprecated aliases of `monocle router ...` (base models):

```bash
monocle router models                                   # pick a base model id
monocle workspace-model create e2e-1 --base <base> --system "Answer in one line." \
  --tool mcp:myserver --knowledge my-fixtures
monocle chat --model e2e-1 "hello"
monocle workspace-model delete e2e-1
```

`<collection>` is an exact name or id; a name shared by several collections is an error that lists their ids. Uploads index each file on the server before the next one starts, so large PDFs take a while. Data goes to stdout and progress to stderr, so output pipes cleanly. Commands act on the selected `--profile`/`--env` tenant.

### 🗂️ Craft sessions

Inspect and clean up Craft sessions from the terminal, on either backend (`--lane`, default `openworker`):

```bash
monocle craft session list [--workspace PATH]       # id, messages, updated, title
monocle craft session show <id>
monocle craft session history <id> [--all]          # openworker: `role<TAB>text` per message (--all adds system); sdk: `type<TAB>text` per event (user/text content, tool name for tool events)
monocle craft session files <id>                    # sdk: [--folder uploads|outputs]
monocle craft session download <id> <path> [-o FILE]  # bytes to stdout, or to FILE
monocle craft session delete <id>
monocle --env stg craft --lane sdk session list     # jarvice agent sessions
monocle craft session delegate <id> [--json]        # openworker only: attach your Stark delegation
monocle craft workspace list [--json]               # openworker only: recent workspaces (name, exists, path)
monocle craft workspace open <path> [--create]      # prints the server's JSON response
monocle craft agent list [--json]                   # openworker only: name, default, needs workspace, title
monocle craft turn "TEXT" [--session ID] [--workspace PATH [--create-workspace]] [--agent NAME] \
    [--timeout SECS] [--no-delegation] [--approve deny|once] [--json]   # openworker: one agent turn
monocle craft --lane sdk turn "TEXT" [--session ID] [--title T] [--timeout SECS] [--json]   # sdk: one jarvice agent-session turn
```

- `openworker` talks to the Craft server with your monocle access token. Its URL is `MONOCLE_CRAFT_URL` if set, else `https://craft-stg.monocle-ai.com` for `--env stg` and `https://craft.monocle-ai.com` for `--env prod`; for `local` set `MONOCLE_CRAFT_URL`. An override is refused if it names the other environment's Craft host (your token is never sent across environments) or any host other than loopback. `--workspace` filters `list` (this lane only).
- `sdk` uses your tenant's jarvice agent sessions (`--folder` on `files` applies here only).
- `list`, `show`, `history`, `files` and `delete` take `--json` for the raw server data on stdout. Data goes to stdout; the profile banner and confirmations go to stderr.
- `workspace`, `agent` and `session delegate` exist on the openworker lane only; `--lane sdk` reports an error.
- `session delegate` lets Craft call the model gateway as you: it sends your stored Stark `id_token` to `POST /v1/sessions/<id>/delegation` and prints `Delegation attached (expires_at N)` to stderr. The token is sent only to the selected environment's own Craft host (`https://craft-stg.monocle-ai.com` for `stg`, `https://craft.monocle-ai.com` for `prod`, default port); `MONOCLE_CRAFT_URL` is refused for this command unless it names exactly that host, and `local` is refused. Redirects are never followed and the token is never printed. A refusal asks you to run `monocle login` again; a 404 means the session belongs to another tenant.
- `craft turn` sends one message over the session WebSocket and prints the answer to stdout (progress, tool lines and `Session: <id>` go to stderr; the id is `--session` or a fresh `cli-<time>-<hex>`, so you can continue it later). Unless `--no-delegation`, it first attaches your delegation exactly like `session delegate`. The workspace is `--workspace` (opened first; `--create-workspace` creates it), else your most recent existing one, else the server default. The agent defaults to `cowork`; `--timeout` (default 180 s) is one overall deadline covering delegation, workspace setup, the socket and polling; it starts after the initial connect and token refresh, which are not counted. Anything the server echoes that contains your token is printed as `[REDACTED]`, even when the token is split across streamed text chunks (the last few characters of the stream are held back briefly to make that possible). A CLI turn cannot answer prompts: permission requests are denied unless `--approve once`, and questions/plans interrupt the turn (non-zero exit). `--json` prints each received frame (`{"source":"ws","frame":...}`) or polled message (`{"source":"poll","message":...}`) as one line instead of text. If the server only accepts turns on the socket (`turn_accepted`, no streaming), the answer is polled from the session's messages every 2 s until the log settles. Your access token travels in the WebSocket subprotocol header, so the socket is only opened to `wss://` on the env's Craft host (default port) or to a loopback address; redirects are never followed and the token is never printed.
- `craft --lane sdk turn` runs one turn on a jarvice agent session: it uses `--session` or creates one (`POST /api/v1/agent/sessions`, title `--title`, default `CLI turn`; `Session: <id>` goes to stderr), starts it over Socket.IO (`/agent` namespace), waits until the sandbox is `active` (other statuses go to stderr), sends the message and streams `text_delta` text to stdout (`text` is used only when no delta arrived). Tool use is one stderr line each, results are not dumped; an `error` event, `reconnect_failed`, an interrupt or `result` with `success=false` exits non-zero. `--timeout` (default 180 s) is one overall deadline covering session creation, the socket-token request, the socket connect, the wait for `active` and the turn; it starts after the initial connect and token refresh, which are not counted. On timeout the CLI sends `interrupt` best-effort. Anything the server echoes that contains your Stark token or the socket token is printed as `[REDACTED]` (stdout, stderr and `--json` lines), even when split across streamed text chunks (the last few characters of the stream are held back briefly to make that possible). `--json` prints each socket event as `{"event":...,"data":...}`. `--workspace` is refused on this lane; `--agent`, `--approve` and `--no-delegation` are ignored. The socket credential is a short-lived jarvice token fetched from `GET /api/v1/auths/` with your Stark bearer; it is kept in memory only, sent solely as the connect auth to `wss://<your tenant host>/ws/socket.io/` (default port; loopback `ws://` only for local dev), never printed or stored, and the connection is refused before any request for any other host. Redirects are never followed.
- `--timings` (`craft turn` on both lanes, and one-shot `monocle chat`) prints ONE JSON line to **stderr** after the turn and never touches stdout, so it works with `--json` and pipes: `{"timings":{"run_id":"…","started_at":"…Z","lane":"openworker|sdk|chat","t_request_ms":…,"ttfe_ms":…,"ttft_ms":…,"total_ms":…,"connect_ms":…,"delegation_ms":…,"session_create_ms":…}}`. **t0 is the moment the user message is handed to the socket** (the HTTP request for `chat`). `ttfe_ms` = first server event/frame after t0, `ttft_ms` = first non-empty assistant text, `total_ms` = turn end (all measured from t0 on a monotonic clock; `null` when it never happened, e.g. no text, or on a failed turn that never got that far — the line is printed on errors too). `t_request_ms` = command start → t0 (setup total; for `chat` this includes auth and reading stdin/attachments); `connect_ms` (socket connect), `delegation_ms` (openworker, unless `--no-delegation`) and `session_create_ms` (sdk, when a session is created) are the setup phases. `chat` is one blocking response, so there `ttfe = ttft = total`; when an openworker answer is polled (`turn_accepted`), `ttft` is accurate to the 2 s poll. `run_id` (random, non-secret) and `started_at` (UTC) correlate a line with server logs; the run id is never sent to any server. The line is built from a fixed set of numeric/enum fields: no token, prompt or answer text can appear in it.
- Server error bodies are never printed: you see the HTTP status and, when the server sends one, a short error code. On the openworker lane `files` reports "session files unavailable" if the session is parked or its sandbox is gone.

### 🧵 Hosted Chat: server-managed threads

Since 1.8, plain `monocle chat` talks to jarvice's custom **Responses API**
(`/api/responses`) instead of the plain `/v1/chat/completions` path (that is
now `monocle router chat`). `--responses` is still accepted as a no-op until 2.0. It's not
OpenAI's Responses API — it's monocle's own endpoint, named for the similar
idea: the **server** persists the conversation thread, so the CLI sends only
the new turn instead of resending the whole exchange.

```bash
monocle chat --model claude-sonnet-4-6
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
echo "and in Python?" | monocle chat --resume 3fa3b2c1-...
```

`/diag` also works in this REPL, but the Responses API doesn't echo back a served
model or token usage, and (being a single blocking call, not streamed) has no
time-to-first-byte to report either — so those lines are omitted, and only
`Endpoint`/`Requested model`/`Latency (total)` are shown.

List your existing threads (including ones started in jarvice's own web UI —
both share the same storage) with the `list` subcommand:

```bash
monocle chat list
```

Print one thread's active message path to stdout with `content` — text only,
attachments omitted, marked as untrusted reference material, and failing
(not truncating) above 1 MiB. Terminal control characters (ESC and other
C0/C1 controls except newline and tab) are stripped, and the thread ID must
match `[A-Za-z0-9_-]{1,128}`. Hand it to a coding agent to continue the work:

```bash
monocle chat content <id> --format json   # or the default Markdown-style text
claude "Run \`monocle chat content <id>\` and continue that work"
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
  it's fully generated, unlike `monocle router chat`'s live token stream.
- **`--system-prompt`/`--system-prompt-file`/`--max-tokens` are ignored** — the
  endpoint has no equivalent fields. Each one you pass prints
  ``Warning: <flag> ignored; use `monocle router chat` to keep the old behaviour``
  to stderr, and the request still goes to hosted Chat.
- **Tool calls aren't executed client-side** — this mode doesn't run a tool
  loop itself; if the model requests a tool this CLI can't run locally, a
  warning is printed to stderr — naming it when its shape parses, or noting
  the count when it doesn't — instead of silently dropping it (see
  [monocle-cli#101](https://github.com/warmblood-kr/monocle-cli/issues/101)).
  `monocle agent` executes tools client-side today, if that's what you need
  right now. This is separate from jarvice's own **server-executed** tools
  (e.g. `web_search`) — those run on jarvice and come back already resolved;
  see `--verify-tool-firing` below to check that they actually did.
- **`--tool-ids <id,...>`** — MCP/connected-app server ids to activate for
  this turn (comma-separated). Without it, a turn only gets whatever tools
  are in the model's own static config — there's no other way to pick which
  MCP servers are on for a given call. Values are sent to jarvice exactly as
  given — CLI does no prefixing — and jarvice only activates entries that
  start with `mcp:`; a bare server id (e.g. `ms365` instead of `mcp:ms365`) is
  not an error, it's **silently dropped**, so zero tools get activated and
  `--verify-tool-firing` reports exit 1 — indistinguishable from the tool not
  being wired up at all unless you already know to check the prefix. Use
  `mcp:{server}` to activate a whole server (confirmed working live below); a
  tool-level form, `mcp:{server}__{tool}`, is also supported in jarvice's code
  but has not been exercised live as of this writing:

  ```bash
  echo "오늘 받은 메일 있나요?" \
    | monocle chat --tool-ids mcp:ms365 --verify-tool-firing
  ```
- **`--verify-tool-firing[=<tool>]`** — one-shot only; asserts that a
  server-executed tool **actually ran**, as a scriptable exit code. Useful
  after a jarvice/chat-proxy deploy, without eyeballing stderr:

  ```bash
  # 이 툴이 돌았는지 (권장)
  echo "search the web for today's date" \
    | monocle chat --verify-tool-firing=web_search
  echo "exit: $?"

  # 아무 툴이나 하나라도 돌았는지 (약한 형태)
  echo "search the web for today's date" | monocle chat --verify-tool-firing
  ```

  **Name the tool you expect.** Bare, this only asks "did *any* tool run", so a
  turn that fired `query_chat_history` when you were checking `web_search`
  still goes green — the check cannot fail in the direction you care about.
  `pytest.raises` takes an exception type for the same reason.

  | exit | `--verify-tool-firing` (bare) | `--verify-tool-firing=web_search` |
  |------|-------------------------------|-----------------------------------|
  | `0` | some tool ran | **`web_search`** ran |
  | `1` | no tool ran, **or** an unresolved tool_calls report leaked back | a different tool ran, or none ran, or a leak |
  | `2` | jarvice did not report `tools_used` (predates the field) | same, **plus**: entries came back unreadable, so whether `web_search` ran is unknown |

  Exit `2` is "could not determine", not "failed". With a named tool, entries
  whose names could not be read report `2` rather than `1` — claiming the tool
  did *not* run would assert something we do not know. Unreadable entries are
  always counted and surfaced in the message, never passed over silently.

  The answer itself is always written to stdout first, whatever the verdict, so
  a failing check never swallows the output you need in order to see why it
  failed.

  Exit `2` is deliberately not folded into `1`. jarvice reports the tools it
  executed in a `tools_used` field
  ([jarvice#1441](https://github.com/warmblood-kr/jarvice/issues/1441)); an
  **empty** `tools_used` is the server stating that nothing ran (a real
  failure), while an **absent** one only means the deployment is older than the
  field — which is not evidence either way. Reporting the second as a pass
  would make this check unable to fail in the direction it claims to check;
  reporting it as a failure would cry wolf through every rollout. Deploy the
  server side first, then re-run.

  Before
  [monocle-cli#118](https://github.com/warmblood-kr/monocle-cli/issues/118)
  this flag only checked that no *unresolved* tool_calls came back, so a prompt
  that fired no tool at all also passed.
- **Known auth gap**: this endpoint currently rejects the CLI's access token
  with a 401 (`JWT missing required claim: email`) against real staging/prod
  tenants — the same open issue that blocks `monocle mcp` today. It's fine to
  use against a local/mock dev stack in the meantime.

## 🔊 Audio (STT / TTS)

`monocle router audio …` calls the audio endpoints directly so you can iterate on parameters and isolate API-level issues without going through a frontend. The OpenAI-compatible and Azure variants are separate subcommands because their parameter schemas don't overlap.

### Transcribe

OpenAI-compatible (`/v1/audio/transcriptions`):

```bash
monocle router audio transcribe meeting.wav --model gpt-4o-mini-transcribe --language en
```

Pipe audio from another tool:

```bash
ffmpeg -i talk.m4a -f wav - | monocle router audio transcribe --filename talk.wav --model gpt-4o-mini-transcribe
```

Azure Fast (`/v1/speechtotext/transcriptions:transcribe`) — for diarization and longer file uploads:

```bash
monocle router audio transcribe-azure meeting.wav \
  --locale en-US --locale ko-KR \
  --diarization \
  --profanity Masked
```

Need an Azure parameter we haven't exposed yet? Use the escape hatch:

```bash
monocle router audio transcribe-azure meeting.wav --definition '{"locales":["ja-JP"],"customSetting":true}'
```

### Speak

OpenAI-compatible (`/v1/audio/speech`):

```bash
monocle router audio speak "Hello from Monocle" --voice nova --format mp3 -o hello.mp3
```

Pipe text in and audio out:

```bash
echo "the quick brown fox" | monocle router audio speak --voice alloy > sample.mp3
```

Azure SSML (`/v1/azure/texttospeech/cognitiveservices/v1`) — body must be SSML (start with `<speak …>`), so it's safer to keep it in a file than to escape it on the shell:

```bash
cat > /tmp/jenny.ssml <<'EOF'
<speak version="1.0" xmlns="http://www.w3.org/2001/10/synthesis" xml:lang="en-US">
  <voice name="en-US-JennyNeural">Hello there.</voice>
</speak>
EOF

monocle router audio speak-azure \
  --format audio-24khz-48kbitrate-mono-mp3 \
  -o jenny.mp3 \
  < /tmp/jenny.ssml
```

On failure each command prints the HTTP status and response body to stderr and exits non-zero, which makes it easy to spot bad parameters or backend errors.

## 🖼️ Image (generate / edit)

`monocle router image …` calls chat-proxy's OpenAI-compatible image endpoints directly, the same way `monocle router audio …` does for STT/TTS. There is no separate "upload a file, then reference it by id" step — an edit sends the image bytes straight in the request, matching chat-proxy's own `/v1/images/edits` contract.

### Generate

`/v1/images/generations`:

```bash
monocle router image generate "a red bicycle on a white background" --model gpt-image-1 -o bike.png
```

Pipe a prompt in from another tool:

```bash
echo "a watercolor mountain landscape" | monocle router image generate --model gpt-image-1 -o landscape.png
```

`--n <n>` greater than 1 writes additional files with `-1`, `-2`, ... spliced before the extension (`bike.png`, `bike-1.png`, ...).

### Edit

`/v1/images/edits` — **gpt-image models only** (Gemini image editing is a separate, non-OpenAI-shaped surface not covered by this subcommand):

```bash
monocle router image edit mockup.png "add a second monitor on the desk" --model gpt-image-1 -o mockup-edited.png
```

With a mask (same image format as `image`: png/jpg/webp):

```bash
monocle router image edit mockup.png "replace the sky" --model gpt-image-1 --mask sky-mask.png -o mockup-edited.png
```

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

### Update notice

While a command runs, monocle checks for a newer release in the background at
most once every 24 hours (3-second timeout, failures ignored; the result is
cached in `~/.monocle/update-check.json`; the command never waits more than
~300 ms for it) and, if one exists, prints a single line to stderr when the
command finishes:

```
monocle X.Y.Z is available (current A.B.C). Run `monocle upgrade`.
```

The notice is never shown when stderr is not a terminal, when `CI` is set, or
for `token`, `upgrade` and `acp`. Set `MONOCLE_NO_UPDATE_CHECK=1` to disable
the check entirely.

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
`/diag` shows diagnostics (served model, endpoint, time to first byte, total latency,
tokens, and step count) for the last turn, `/model` shows the current model (or `/model <id>` switches it for later
turns), and `/exit` (or `/quit`, Ctrl-D) quits. `/model <TAB>` fuzzy-completes against the
model ids available to your account (fetched once at startup) — e.g. `/model cla<TAB>`
narrows to matching ids, and a bare `/model <TAB>` lists them all; if you're not logged in
or the list can't be fetched, completion just offers nothing (typing a full id still
works).

```bash
monocle agent "summarize the TODOs in this repo" --workdir .
```

> ⚠️ In an interactive session, each side-effecting tool call (write/edit + shell) asks
> for confirmation before it runs, with four choices — **`y`** (allow once),
> **`s`** (allow for the rest of this session), **`a`** (allow always), or **`N`**
> (deny, the default; anything unrecognized also denies). Choosing `s` or `a` means
> that tool won't be asked about again — `s` for the current session only, `a`
> persisted to `.monocle/settings.json` in the working directory so future sessions
> skip it too. Granularity is per tool, except the shell, which is remembered per
> command (allowing `npm test` doesn't green-light every shell command). Pass
> `--auto-approve` to skip the prompt entirely (dangerous). Non-interactive runs (a
> prompt argument or piped stdin) have no TTY to prompt on and run tools unattended,
> so run those only in a directory you trust.
>
> `.monocle/settings.json` is a plain JSON file you can hand-edit; `allowedTools`
> is a list of rules like `"write_file"`, `"edit_file"`, `"bash(npm test)"`, or a
> `*`-suffixed prefix `"bash(cargo *)"` to allow a whole command family:
>
> ```json
> { "allowedTools": ["edit_file", "bash(cargo *)"] }
> ```

**Guide files (`AGENTS.md`).** At startup `monocle agent` reads a guide file and
appends it to the system prompt, so personal- and project-level instructions steer
the agent. Two locations load in order — your personal `~/.monocle/` (applies
everywhere), then the working directory (project-specific, loaded last so it wins
on conflict). Each loaded guide is noted on stderr. This seeds a fresh session; a
resumed `--session` replays its own saved prompt.

In each location the **first** of these names that exists is used — at most one
file per directory:

| Priority | File | Why |
|---|---|---|
| 1 | `AGENTS.md` | the cross-tool open convention (Codex, Cursor, Amp, Jules, …) |
| 2 | `AGENT.md` | Amp's fallback spelling |
| 3 | `CLAUDE.md` | Claude Code |
| 4 | `GEMINI.md` | Gemini CLI |

So a repo that already has any of these works with no new file. (There is no
`CODEX.md` — Codex reads `AGENTS.md`.)

**Imports.** `AGENTS.md` itself is plain Markdown with no preprocessing, but
`@path` imports are a common extension; we follow [Claude Code's
semantics](https://code.claude.com/docs/en/memory), the most precisely specified:

```markdown
See @docs/style.md for conventions and @~/.monocle/shared-rules.md for mine.
```

- `@path/to/file` is expanded anywhere **outside** code spans and fenced blocks
- relative paths resolve against **the importing file's own directory**; `~/` is
  home; absolute paths work
- imported files may import further, up to **4 hops** deep
- backtick-wrap to escape: `` `@README` `` stays literal
- a missing import is left in place as written

Guide files support **no command execution** — imports are the only
preprocessing (same as Claude Code's CLAUDE.md; `!`-style bash execution is a
slash-command feature, not a memory-file one).

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
