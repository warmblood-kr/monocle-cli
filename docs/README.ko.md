# Monocle CLI

[English](../README.md)

> 터미널에서 [Monocle AI](https://monocle-ai.com)를 제어하고 사용하는 유틸리티예요. 한 번 로그인하면 그 세션으로 모델과 채팅하고, Claude Code를 연동하고, 직접 만든 앱에서 Monocle의 OpenAI 호환 API를 호출할 수 있어요.

## 시작하기 전에

- **Node.js** 18 이상 — `node -v`로 확인해 주세요

## 🚀 설정

```bash
npm install -g @warmblood/monocle-cli
monocle login
```

브라우저가 열리면 회사 계정으로 로그인해 주세요.

## ✅ 상태 확인

```bash
monocle status
```

테넌트, 사용자, 액세스/리프레시 토큰 유효성, Claude Code 전역 연동 여부를 보여줘요. 읽기 전용이라 토큰 갱신은 하지 않아요.

## 📖 전체 명령어

| 명령어 | 설명 |
|--------|------|
| `monocle login [--tenant <domain>] [--device-code]` | 로그인 |
| `monocle status` | 로그인, 토큰, Claude Code 구성 상태 확인 |
| `monocle token` | 현재 액세스 토큰 출력 (만료 임박 시 자동 갱신) |
| `monocle model list` | 사용 가능한 모델 목록 |
| `monocle model chat [--model <id>] [--system-prompt <text>] [--system-prompt-file <path>] [--max-tokens <n>]` | 모델과 채팅 (REPL 또는 stdin) |
| `monocle claude [...args]` | Claude Code를 Monocle에 연결해서 실행 (인자 그대로 전달) |
| `monocle setup` | 일반 `claude`도 전역으로 Monocle을 거치게 설정 (opt-in) |
| `monocle unset` | 전역 `claude` 라우팅 제거 |

## 💬 모델과 채팅하기

사용 가능한 모델 보기:

```console
$ monocle model list
MODEL ID              NAME                  OWNER       CONTEXT
────────────────────  ────────────────────  ──────────  ─────────
claude-sonnet-4-6     Claude Sonnet 4.6     anthropic   200k
claude-opus-4-7       Claude Opus 4.7       anthropic   200k
gpt-4o                GPT-4o                openai      128k

3 model(s) available.
```

인터랙티브 REPL:

```console
$ monocle model chat --model claude-sonnet-4-6
Monocle Chat (model: claude-sonnet-4-6)
Router: https://api.monocle-ai.com
Type your message. Press Ctrl+D to exit.
---
> 안녕하세요
안녕하세요! 무엇을 도와드릴까요?

> /quit
Bye.
```

stdin 파이프로 one-shot:

```console
$ echo "OAuth 2.0을 한 문장으로 요약해줘." | monocle model chat
Using model: claude-sonnet-4-6
Router: https://api.monocle-ai.com
OAuth 2.0은 사용자가 비밀번호를 공유하지 않고도 애플리케이션이 다른 서비스의 자원에 접근할 수 있게 해주는 인증 프레임워크예요.
```

파일에서 시스템 프롬프트를 불러와서:

```bash
monocle model chat --system-prompt-file ./persona.md --model claude-opus-4-7
```

## 🤖 Claude Code 연동

```bash
monocle claude
```

다른 터미널이나 IDE 연동에서 실행되는 일반 `claude`는 영향받지 않아요. 일반 `claude`도 전역으로 Monocle을 거치게 하려면 `monocle setup`을 한 번 실행하세요. 해제는 `monocle unset`.

> [!NOTE]
> 자세한 내용은 **[Claude Code 연동 상세](./claude-code.ko.md)** — `ANTHROPIC_API_KEY` 처리, 전역 설정, 문제 해결을 참고해 주세요.

## 🔌 직접 만든 앱에서 쓰기 (OpenAI 호환 SDK)

Monocle은 OpenAI 호환 Chat Completions API를 제공해요. 환경 변수 두 개만 설정하면 어떤 OpenAI 클라이언트로도 호출할 수 있어요. 한 번 export 해두세요:

```bash
export MONOCLE_API_KEY="$(monocle token)"
export MONOCLE_BASE_URL="$(jq -r .router_url ~/.monocle/credentials.json)/v1"
```

그리고 Python에서:

```python
import os
from openai import OpenAI

client = OpenAI(
    api_key=os.environ["MONOCLE_API_KEY"],
    base_url=os.environ["MONOCLE_BASE_URL"],
)

resp = client.chat.completions.create(
    model="claude-sonnet-4-6",
    messages=[{"role": "user", "content": "안녕"}],
)
print(resp.choices[0].message.content)
```

> [!NOTE]
> Node.js, `curl`, 스트리밍, 장시간 실행 앱에서의 토큰 갱신 패턴, 문제 해결은 **[OpenAI SDK로 Monocle 사용하기](./openai-sdk.ko.md)** 참고.

## 🆘 문제가 생겼나요?

**"Not logged in" 오류가 나와요**
→ `monocle login`을 먼저 실행해 주세요.

**토큰이 만료됐대요**
→ `monocle token`이 만료 임박 시 자동으로 갱신해요. 마지막 로그인 후 30일이 지났다면 `monocle login`을 다시 실행해 주세요.

Claude Code나 OpenAI SDK 관련 문제는 위의 연결된 가이드를 참고해 주세요.

## 도움이 필요하신가요?

조직의 관리자에게 문의하시거나 이 저장소에 이슈를 등록해 주세요.
