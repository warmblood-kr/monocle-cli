# Using Monocle with the OpenAI SDK

[한국어](./openai-sdk.ko.md)

Monocle exposes an **OpenAI-compatible** Chat Completions API, so any tool or
library that speaks the OpenAI protocol can talk to Monocle with two pieces of
configuration:

1. A **base URL** that points at the Monocle router.
2. An **access token** issued by Monocle OIDC login.

This guide shows how to obtain both using the `monocle` CLI and how to wire
them into the official OpenAI SDKs (Python and Node.js) or a plain HTTP
client.

---

## Prerequisites

- **Node.js** 18+ (required to install and run the CLI).
- A Monocle tenant you can log in to (e.g. `your-org.monocle-ai.com`).

Install the CLI and log in:

```bash
npm install -g @warmblood/monocle-cli
monocle login
```

Verify:

```bash
monocle status
```

---

## Where your credentials live

After `monocle login` succeeds, the CLI writes your credentials to:

```
~/.monocle/credentials.json   (file mode 0600)
```

The file contains (abridged):

```json
{
  "tenant_domain": "your-org.monocle-ai.com",
  "tenant_name": "your-org",
  "email": "you@your-org.com",
  "access_token": "<JWT>",
  "refresh_token": "<opaque>",
  "id_token": "<JWT>",
  "access_token_expires_at": "2026-04-19T12:00:00.000Z",
  "refresh_token_expires_at": "2026-05-19T12:00:00.000Z",
  "router_url": "https://api.monocle-ai.com"
}
```

- `access_token` is a short-lived JWT (RFC 9068 `at+JWT`) — you pass it to the
  API as `Authorization: Bearer <access_token>`.
- `router_url` is your **base URL**. It is set by OIDC discovery during
  login; for production tenants this is typically
  `https://api.monocle-ai.com`.
- `refresh_token` is used to mint a new access token when the current one
  expires. The CLI handles this for you (see below).

---

## Getting a token for your SDK

You have two options. Pick based on how your code runs.

### Option 1 (recommended) — `monocle token`

The CLI prints the current access token to stdout, refreshing it
automatically if it is within 5 minutes of expiry:

```bash
monocle token
```

Use it from shell:

```bash
export MONOCLE_API_KEY="$(monocle token)"
export MONOCLE_BASE_URL="$(jq -r .router_url ~/.monocle/credentials.json)"
```

Or from application code by shelling out before each request. This is the
simplest way to stay correct — you inherit the CLI's refresh logic for free.

### Option 2 — read `credentials.json` directly

Useful when you want to avoid spawning a subprocess (e.g. in a long-running
server). You read the file, use `access_token`, and re-read when a request
returns **401**. If `access_token_expires_at` has passed, either call
`monocle token` to refresh or implement refresh against the OIDC
`token_endpoint` yourself. For most apps, **Option 1 is simpler and
correct**.

---

## Listing available models

Monocle exposes `GET /v1/models` (OpenAI-compatible). The CLI wraps it:

```bash
monocle model list
```

Example output:

```
MODEL ID              NAME                  OWNER       CONTEXT
────────────────────  ────────────────────  ──────────  ─────────
claude-sonnet-4-6     Claude Sonnet 4.6     anthropic   200k
claude-opus-4-7       Claude Opus 4.7       anthropic   200k
gpt-4o                GPT-4o                openai      128k
```

The `MODEL ID` column is the value you pass as `model` to the SDK.

---

## Python — OpenAI SDK

```bash
pip install openai
export MONOCLE_API_KEY="$(monocle token)"
export MONOCLE_BASE_URL="$(jq -r .router_url ~/.monocle/credentials.json)"
```

```python
import os
from openai import OpenAI

client = OpenAI(
    api_key=os.environ["MONOCLE_API_KEY"],
    base_url=f"{os.environ['MONOCLE_BASE_URL']}/v1",
)

resp = client.chat.completions.create(
    model="claude-sonnet-4-6",
    messages=[
        {"role": "system", "content": "You are a concise assistant."},
        {"role": "user", "content": "Summarize OAuth 2.0 in one sentence."},
    ],
)

print(resp.choices[0].message.content)
```

Streaming works the same as with OpenAI:

```python
stream = client.chat.completions.create(
    model="claude-sonnet-4-6",
    messages=[{"role": "user", "content": "Count to five."}],
    stream=True,
)
for chunk in stream:
    delta = chunk.choices[0].delta.content
    if delta:
        print(delta, end="", flush=True)
```

---

## Node.js / TypeScript — OpenAI SDK

```bash
npm install openai
```

```ts
import OpenAI from "openai";
import { execSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const apiKey = execSync("monocle token").toString().trim();
const creds = JSON.parse(
  readFileSync(join(homedir(), ".monocle", "credentials.json"), "utf8"),
);
const baseURL = `${creds.router_url}/v1`;

const client = new OpenAI({ apiKey, baseURL });

const resp = await client.chat.completions.create({
  model: "claude-sonnet-4-6",
  messages: [{ role: "user", content: "Say hello in one word." }],
});

console.log(resp.choices[0].message.content);
```

---

## Plain HTTP

The API is OpenAI-compatible, so `curl` works directly:

```bash
curl https://api.monocle-ai.com/v1/chat/completions \
  -H "Authorization: Bearer $(monocle token)" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-sonnet-4-6",
    "messages": [{"role": "user", "content": "ping"}]
  }'
```

---

## Keeping tokens fresh in long-running apps

Access tokens expire (typically within the hour). Three patterns:

1. **Shell out per request** — call `monocle token` every time you need a
   token. It is fast and always returns a valid token. Simplest.
2. **Cache and refresh on 401** — cache the token, and on a `401
   Unauthorized` response, call `monocle token` again and retry once.
3. **Implement OIDC refresh yourself** — POST the `refresh_token` to the
   OIDC `token_endpoint`. Only needed if you cannot run the CLI in your
   environment.

`monocle login` keeps the refresh token valid for 30 days. After that, the
user must log in again interactively.

---

## Troubleshooting

**`401 Unauthorized`**
→ Your access token expired or was rejected. Run `monocle status`; if it's
expired, run `monocle token` (auto-refresh) or `monocle login` (full
re-login).

**`404 model_not_found`**
→ The `model` ID you passed isn't available on your tenant. Run `monocle
model list` to see valid IDs.

**`Not logged in`**
→ Run `monocle login` first. On headless / CI environments use `monocle
login --device-code`.

**Connection errors to the base URL**
→ Confirm `router_url` in `~/.monocle/credentials.json` matches what your
admin gave you. Production tenants typically use
`https://api.monocle-ai.com`.
