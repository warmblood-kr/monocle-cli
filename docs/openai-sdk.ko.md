# OpenAI SDK로 Monocle 사용하기

[English](./openai-sdk.md)

Monocle은 **OpenAI 호환** Chat Completions API를 제공해요. OpenAI 프로토콜을
지원하는 도구나 라이브러리라면 두 가지만 설정해 주면 바로 Monocle과 통신할
수 있어요.

1. Monocle 라우터를 가리키는 **base URL**
2. Monocle OIDC 로그인으로 발급받은 **액세스 토큰**

이 문서에서는 `monocle` CLI로 이 두 가지를 얻어서 공식 OpenAI SDK
(Python, Node.js)나 단순 HTTP 클라이언트에 연결하는 방법을 안내해요.

---

## 시작하기 전에

- **Node.js** 18 이상 (CLI 설치/실행에 필요해요)
- 로그인할 수 있는 Monocle 테넌트 (예: `your-org.monocle-ai.com`)

CLI 설치 후 로그인:

```bash
npm install -g @warmblood/monocle-cli
monocle login
```

상태 확인:

```bash
monocle status
```

---

## 인증 정보가 저장되는 위치

`monocle login`이 성공하면 CLI가 아래 경로에 인증 정보를 저장해요.

```
~/.monocle/credentials.json   (파일 권한 0600)
```

파일 내용은 다음과 같아요 (일부 생략):

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

- `access_token`은 수명이 짧은 JWT(RFC 9068 `at+JWT`)예요. API 호출 시
  `Authorization: Bearer <access_token>` 헤더로 전달해요.
- `router_url`이 **base URL**이에요. 로그인 시 OIDC discovery로 설정되며,
  프로덕션 테넌트의 경우 보통 `https://api.monocle-ai.com`이에요.
- `refresh_token`은 액세스 토큰이 만료되었을 때 새 토큰을 발급받는 데
  사용해요. CLI가 자동으로 처리해 주니 아래 섹션을 참고하세요.

---

## SDK에 전달할 토큰 얻기

두 가지 방법이 있어요. 코드가 어떻게 실행되는지에 따라 골라 쓰세요.

### 방법 1 (권장) — `monocle token`

CLI가 현재 액세스 토큰을 stdout으로 출력해 주는데, 만료 5분 전이면 자동으로
갱신해서 돌려줘요.

```bash
monocle token
```

셸에서 쓰기:

```bash
export MONOCLE_API_KEY="$(monocle token)"
export MONOCLE_BASE_URL="$(jq -r .router_url ~/.monocle/credentials.json)"
```

애플리케이션 코드에서 요청 직전마다 이 명령을 호출해서 토큰을 받아 써도 돼요.
CLI의 자동 갱신 로직을 그대로 재사용하기 때문에 가장 안전하고 간단한
방법이에요.

### 방법 2 — `credentials.json`을 직접 읽기

서브프로세스를 띄우고 싶지 않을 때(예: 장시간 실행되는 서버) 유용해요.
파일을 읽어서 `access_token`을 쓰고, 요청이 **401**을 반환하면 다시 읽는
방식이에요. `access_token_expires_at`이 이미 지났다면 `monocle token`을
호출해서 갱신하거나, OIDC `token_endpoint`에 직접 요청해서 갱신 로직을
구현해야 해요. 대부분의 앱에서는 **방법 1이 더 간단하고 안전해요**.

---

## 사용 가능한 모델 목록 보기

Monocle은 OpenAI 호환 `GET /v1/models` 엔드포인트를 제공해요. CLI가 이를
감싸서 보여줘요.

```bash
monocle model list
```

출력 예시:

```
MODEL ID              NAME                  OWNER       CONTEXT
────────────────────  ────────────────────  ──────────  ─────────
claude-sonnet-4-6     Claude Sonnet 4.6     anthropic   200k
claude-opus-4-7       Claude Opus 4.7       anthropic   200k
gpt-4o                GPT-4o                openai      128k
```

`MODEL ID` 컬럼에 있는 값이 SDK에 `model`로 전달할 값이에요.

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
        {"role": "system", "content": "너는 간결한 어시스턴트야."},
        {"role": "user", "content": "OAuth 2.0을 한 문장으로 요약해 줘."},
    ],
)

print(resp.choices[0].message.content)
```

스트리밍도 OpenAI와 똑같은 방식으로 쓸 수 있어요.

```python
stream = client.chat.completions.create(
    model="claude-sonnet-4-6",
    messages=[{"role": "user", "content": "1부터 5까지 세어 줘."}],
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
  messages: [{ role: "user", content: "한 단어로 인사해 줘." }],
});

console.log(resp.choices[0].message.content);
```

---

## 순수 HTTP

OpenAI 호환 API이기 때문에 `curl`로도 바로 호출할 수 있어요.

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

## 장시간 실행 앱에서 토큰을 갱신하는 방법

액세스 토큰은 보통 한 시간 이내에 만료돼요. 세 가지 패턴 중에서 고르세요.

1. **요청마다 셸 호출** — 필요할 때마다 `monocle token`을 실행해요. 빠르고
   언제나 유효한 토큰을 돌려줘서 가장 간단해요.
2. **캐시 + 401 시 갱신** — 토큰을 캐시해 두고, 응답이 `401 Unauthorized`면
   `monocle token`을 다시 호출해서 한 번 재시도하는 방식이에요.
3. **OIDC refresh를 직접 구현** — `refresh_token`을 OIDC `token_endpoint`에
   POST해서 새 토큰을 받아와요. CLI를 실행할 수 없는 환경에서만 필요해요.

`monocle login`이 발급하는 refresh token은 30일 동안 유효해요. 그 이후에는
사용자가 다시 대화형으로 로그인해야 해요.

---

## 문제가 생겼나요?

**`401 Unauthorized`**
→ 액세스 토큰이 만료됐거나 거부된 거예요. `monocle status`로 확인하시고,
만료되었다면 `monocle token`(자동 갱신) 혹은 `monocle login`(재로그인)을
실행해 주세요.

**`404 model_not_found`**
→ 전달한 `model` ID가 테넌트에 없어요. `monocle model list`로 사용 가능한
모델 ID를 확인해 주세요.

**`Not logged in`**
→ `monocle login`을 먼저 실행해 주세요. 헤드리스/CI 환경이면
`monocle login --device-code`를 쓰시면 돼요.

**base URL 연결 오류**
→ `~/.monocle/credentials.json`의 `router_url`이 관리자가 알려 준 값과
같은지 확인해 주세요. 프로덕션 테넌트는 보통
`https://api.monocle-ai.com`이에요.
