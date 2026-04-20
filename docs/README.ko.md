# Monocle CLI

[English](../README.md)

Monocle CLI는 터미널에서 [Monocle AI](https://monocle-ai.com)를 제어하고 사용하는 유틸리티예요. 한 번 로그인하면 그 세션으로 채팅, 모델 조회, Claude Code 연동, 직접 만든 앱에서 Monocle의 OpenAI 호환 API 호출까지 이어서 쓸 수 있어요.

## 시작하기 전에

- **Node.js** 18 이상 — `node -v`로 확인해 주세요

## 설정 (30초면 충분해요)

**Step 1** — 설치

```bash
npm install -g @warmblood/monocle-cli
```

**Step 2** — 로그인

```bash
monocle login
```

브라우저가 열리면 회사 계정으로 로그인해 주세요. OAuth 흐름이 끝나면 Monocle CLI가 자격 증명을 `~/.monocle/credentials.json`(권한 0600)에 저장해요 — 짧은 수명의 액세스 토큰과 30일짜리 리프레시 토큰이 함께 들어가요.

> **Tip:** 특정 테넌트를 지정하려면 `monocle login --tenant your-org.monocle-ai.com`.
>
> **헤드리스 / SSH / CI 환경이신가요?** `monocle login`이 자동 감지해서 device code 방식으로 전환해요. 수동으로 강제하려면 `--device-code`.

## 상태 확인

```bash
monocle status
```

테넌트, 사용자, 액세스/리프레시 토큰 유효성, Claude Code 전역 연동 여부를 보여줘요. 이 명령은 읽기 전용이에요 — 토큰 갱신은 하지 않아요 (갱신은 `monocle token` 호출이나 연동 사용 시에 자동으로 일어나요).

## 전체 명령어

| 명령어 | 설명 |
|--------|------|
| `monocle login [--tenant <domain>] [--env <env>] [--device-code]` | 로그인 — 기본은 브라우저, 헤드리스/SSH/CI에서는 device code |
| `monocle status` | 로그인, 토큰 유효성, Claude Code 구성 상태 확인 |
| `monocle token` | 현재 액세스 토큰 출력 (만료 임박 시 자동 갱신) |
| `monocle model list` | 테넌트에서 사용 가능한 모델 목록 |
| `monocle model chat [--model <id>] [--system-prompt <text>] [--system-prompt-file <path>] [--max-tokens <n>]` | 모델과 채팅 — 인터랙티브 REPL 또는 stdin 파이프 |
| `monocle claude [...args]` | Monocle 설정을 **이 실행에만** 적용해서 Claude Code 실행 (인자 그대로 전달) |
| `monocle setup` | 일반 `claude`도 전역으로 Monocle을 거치게 설정 (opt-in) |
| `monocle unset` | 전역 `claude` 라우팅 제거 |

## 모델과 채팅하기 (`monocle model`)

사용 가능한 모델 보기:

```bash
monocle model list
```

인터랙티브 REPL:

```bash
monocle model chat --model claude-sonnet-4-6
```

stdin 파이프로 one-shot:

```bash
echo "OAuth 2.0을 한 문장으로 요약해줘." | monocle model chat
```

파일에서 시스템 프롬프트를 불러와서:

```bash
monocle model chat --system-prompt-file ./persona.md --model claude-opus-4-7
```

## Claude Code 연동

Monocle 설정을 **이 실행에만** 적용해서 Claude Code 실행:

```bash
monocle claude
```

다른 터미널이나 IDE 연동의 일반 `claude`는 영향받지 않아요. 일반 `claude`(IDE 포함)도 전역으로 Monocle을 거치게 하려면 `monocle setup`을 한 번 실행하세요. 해제는 `monocle unset`.

자세한 내용은 **[Claude Code 연동 상세](./claude-code.ko.md)** — `ANTHROPIC_API_KEY` 처리, 전역 설정, 문제 해결을 참고해 주세요.

## 직접 만든 앱에서 쓰기 (OpenAI 호환 SDK)

Monocle은 OpenAI 호환 Chat Completions API를 제공해요. 두 가지만 설정하면 어떤 OpenAI 클라이언트로도 호출할 수 있어요:

- **Base URL** — 자격 증명의 `router_url` (보통 `https://api.monocle-ai.com`)
- **API key** — `monocle token` 출력값

최소 Python 예시:

```python
import subprocess
from openai import OpenAI

token = subprocess.check_output(["monocle", "token"], text=True).strip()
client = OpenAI(api_key=token, base_url="https://api.monocle-ai.com/v1")

resp = client.chat.completions.create(
    model="claude-sonnet-4-6",
    messages=[{"role": "user", "content": "안녕"}],
)
print(resp.choices[0].message.content)
```

Node.js, `curl`, 스트리밍, 장시간 실행 앱에서의 토큰 갱신 패턴, 문제 해결은 **[OpenAI SDK로 Monocle 사용하기](./openai-sdk.ko.md)** 참고.

## 문제가 생겼나요?

**"Not logged in" 오류가 나와요**
→ `monocle login`을 먼저 실행해 주세요.

**토큰이 만료됐대요**
→ `monocle token`이 만료 임박 시 자동으로 갱신해요. 마지막 로그인 후 30일(리프레시 토큰 TTL)이 지났다면 `monocle login`을 다시 실행해 주세요.

Claude Code나 OpenAI SDK 관련 문제는 위의 연결된 가이드를 참고해 주세요.

## 도움이 필요하신가요?

조직의 관리자에게 문의하시거나 이 저장소에 이슈를 등록해 주세요.
