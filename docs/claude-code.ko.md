# Claude Code 연동

[English](./claude-code.md)

Monocle CLI로 [Claude Code](https://docs.anthropic.com/en/docs/claude-code)를 Anthropic API 대신 Monocle 테넌트에 연결해서 쓸 수 있어요. 두 가지 모드가 있습니다 — **이 실행에만 적용**(기본, 권장)과 **전역 opt-in**.

## 시작하기 전에

- `monocle login`이 먼저 완료되어 있어야 해요 ([메인 README](../README.md#-설정) 참고)
- **Claude Code** 설치 — [여기서 설치](https://docs.anthropic.com/en/docs/claude-code/getting-started)

## 이 실행에만 적용: `monocle claude`

```bash
monocle claude
```

Monocle 설정을 **이 실행에만** 적용한 상태로 Claude Code를 띄워요 (실행 단위 `--settings` 오버라이드). 전역 Claude Code 설정은 건드리지 않아서, 다른 터미널이나 IDE 연동의 `claude`는 그대로예요.

추가 인자는 Claude Code로 그대로 전달돼요:

```bash
monocle claude --help
monocle claude -c      # 가장 최근 세션 재개
```

### `ANTHROPIC_API_KEY` 처리

환경에 `ANTHROPIC_API_KEY`가 설정되어 있으면 Claude Code는 이를 Monocle보다 우선해요. `monocle claude`는 자식 프로세스 환경에서 이 변수를 자동으로 제거해서 Monocle 자격 증명이 사용되도록 해줘요 — 수동으로 `unset` 할 필요 없어요.

## 전역 opt-in: `monocle setup`

다른 터미널이나 IDE 연동의 일반 `claude`도 전부 Monocle을 거치게 하려면:

```bash
monocle setup
```

`~/.claude/settings.json`에 `apiKeyHelper: "monocle token"`을 적어 넣어요. 이후 Claude Code는 요청마다 `monocle token`을 호출해서 최신 액세스 토큰을 가져가요.

해제할 땐:

```bash
monocle unset
```

상태 확인:

```bash
monocle status   # "Claude Code: Configured"
```

## 문제가 생겼나요?

**Claude Code가 Monocle을 무시해요**
→ 환경 변수에 `ANTHROPIC_API_KEY`가 있으면 Monocle보다 우선 적용돼요. `monocle claude`로 실행하면 자동으로 제거해 주거나, 쉘에서 직접 `unset ANTHROPIC_API_KEY` 해주세요.

**일반 `claude`가 Monocle을 안 써요**
→ 의도된 동작이에요. `monocle claude`는 자기 실행에만 Monocle을 적용해요. IDE 연동이나 다른 터미널의 일반 `claude`도 Monocle로 가게 하려면 `monocle setup`을 한 번 실행해 주세요.

**처음부터 다시 하고 싶어요**
→ `monocle unset`으로 전역 라우팅 제거 후, 필요하면 `monocle setup`을 다시 실행하세요.
