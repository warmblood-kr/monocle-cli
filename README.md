# Monocle CLI

Monocle CLI를 사용하면 [Claude Code](https://docs.anthropic.com/en/docs/claude-code)를 Monocle AI에 연동해 사용할 수 있어요.

한 번만 로그인하고 설정하면 끝! API 키 관리도, 설정 파일 직접 수정도 필요 없어요.

## 시작하기 전에

- **Node.js** 18 이상 — `node -v`로 확인해 주세요
- **Claude Code** — 아직 없다면 [여기서 설치](https://docs.anthropic.com/en/docs/claude-code/getting-started)해 주세요

## 설정 (30초면 충분해요)

**Step 1** — Monocle CLI 설치

```bash
npm install -g @warmblood/monocle-cli
```

**Step 2** — 로그인

```bash
monocle login
```

브라우저가 열리면 평소 사용하시는 회사 계정으로 로그인해 주세요.
로그인이 완료되면 Claude Code 설정도 자동으로 진행됩니다.

끝이에요! 이제 `monocle claude`를 실행하시면 됩니다.

> **Tip:** 특정 테넌트를 지정하려면 `monocle login --tenant your-org.monocle-ai.com`으로 실행하세요.
>
> **Tip:** `ANTHROPIC_API_KEY` 같은 환경 변수가 설정되어 있어도 `monocle claude`가 자동으로 정리해 줘요.

## 상태 확인

```bash
monocle status
```

모두 **Valid**이고 **Configured**로 표시되면 정상이에요!

## 전체 명령어

| 명령어 | 설명 |
|--------|------|
| `monocle login [--tenant <domain>] [--env <env>]` | 브라우저로 로그인 (Claude Code 자동 설정 포함) |
| `monocle setup` | Claude Code를 수동으로 설정 (보통은 login이 자동 처리) |
| `monocle claude` | 환경 변수 충돌을 자동 정리하고 Claude Code 실행 |
| `monocle status` | 로그인/설정 상태 확인 |
| `monocle token` | 현재 액세스 토큰 출력 (Claude Code가 내부적으로 사용) |
| `monocle unset` | Claude Code에서 Monocle 설정 제거 |

## 문제가 생겼나요?

**"Not logged in" 오류가 나와요**
→ `monocle login`을 먼저 실행해 주세요.

**토큰이 만료됐대요**
→ 토큰은 자동으로 갱신돼요. 마지막 로그인 후 30일이 지났다면 `monocle login`을 다시 실행해 주세요.

**자동 설정이 실패했대요**
→ `monocle setup`을 수동으로 실행해 주세요.

**Claude Code가 Monocle을 무시해요**
→ 환경 변수에 `ANTHROPIC_API_KEY`가 설정되어 있으면 Monocle보다 우선 적용돼요. `monocle claude`로 실행하면 자동으로 해결됩니다.

**처음부터 다시 하고 싶어요**
→ `monocle unset` 후 `monocle setup`을 다시 실행하면 돼요.

## 도움이 필요하신가요?

조직의 관리자에게 문의하시거나 이 저장소에 이슈를 등록해 주세요.