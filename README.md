# Monocle CLI

Monocle CLI를 사용하면 [Claude Code](https://docs.anthropic.com/en/docs/claude-code)를 우리 조직의 관리형 엔드포인트를 통해 사용할 수 있어요.

한 번만 로그인하고 설정하면 끝! API 키 관리도, 설정 파일 직접 수정도 필요 없어요.

## 시작하기 전에

- **Node.js** 18 이상 — `node -v`로 확인해 주세요
- **Claude Code** — 아직 없다면 [여기서 설치](https://docs.anthropic.com/en/docs/claude-code/getting-started)해 주세요
- **테넌트 도메인** — `your-org.monocle-ai.com` 같은 주소예요 (모르시면 관리자에게 문의해 주세요!)

## 설정 (30초면 충분해요)

**Step 1** — Monocle CLI 설치

```bash
npm install -g @monocle-ai/cli
```

**Step 2** — 로그인

```bash
monocle login --tenant your-org.monocle-ai.com
```

브라우저가 열리면 평소 사용하시는 회사 계정으로 로그인해 주세요.

**Step 3** — Claude Code 연결

```bash
monocle setup
```

끝이에요! 이제 평소처럼 `claude`를 실행하시면 됩니다.

## 상태 확인

```bash
monocle status
```

모두 **Valid**이고 **Configured**로 표시되면 정상이에요!

## 전체 명령어

| 명령어 | 설명 |
|--------|------|
| `monocle login --tenant <domain>` | 브라우저로 로그인 |
| `monocle setup` | Claude Code를 조직 엔드포인트에 연결 |
| `monocle status` | 로그인/설정 상태 확인 |
| `monocle token` | 현재 액세스 토큰 출력 (Claude Code가 내부적으로 사용) |
| `monocle unset` | Claude Code에서 Monocle 설정 제거 |

## 문제가 생겼나요?

**"Not logged in" 오류가 나와요**
→ `monocle login --tenant <domain>`을 먼저 실행한 다음 `monocle setup`을 다시 해 주세요.

**토큰이 만료됐대요**
→ 토큰은 자동으로 갱신돼요. 마지막 로그인 후 30일이 지났다면 `monocle login`을 다시 실행해 주세요.

**Claude Code가 Monocle을 무시해요**
→ 환경 변수에 `ANTHROPIC_API_KEY`가 설정되어 있으면 Monocle보다 우선 적용돼요. 아래 명령어로 해제해 주세요:

```bash
unset ANTHROPIC_API_KEY
unset ANTHROPIC_AUTH_TOKEN
```

**처음부터 다시 하고 싶어요**
→ `monocle unset` 후 `monocle setup`을 다시 실행하면 돼요.

## 도움이 필요하신가요?

조직의 관리자에게 문의하시거나 이 저장소에 이슈를 등록해 주세요.