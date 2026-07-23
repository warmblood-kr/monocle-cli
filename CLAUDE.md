# Monocle CLI

## Stack
- Rust 2021, 단일 standalone 바이너리 (`monocle`) — Node/npm 불필요
- clap (CLI), reqwest(blocking, rustls-tls), serde — 모든 네트워크 I/O는
  `src/net.rs` 파사드 뒤에 격리 (sync 유지, 추후 async 전환은 양방향 문)
- 배포: `install.sh` / `install.ps1` + GitHub Releases (prebuilt 바이너리)

## Commands
```bash
cargo build --release   # 릴리스 빌드 → target/release/monocle
cargo test              # 테스트 실행
cargo clippy --all-targets -- -D warnings   # 린트
cargo fmt               # 포매팅
```

## Conventions
- 커밋 메시지, PR, 이슈: 한국어
- 브랜치 전략: `feature branch → devel → staging → main`
- 버전: `Cargo.toml`의 `version` 한 곳 (릴리스 태그 `vX.Y.Z`와 일치해야 CI 통과)
- 자격증명 파일(`~/.monocle/credentials.json`)은 drop-in 호환 계약 — JSON 스키마/키
  순서/0600 권한을 바꾸지 말 것 (기존 로그인 사용자가 재인증하지 않도록)

## 인증 · 자격증명

> ⚠️ **자격증명 슬롯은 하나뿐이다 — 환경 전환은 현재 세션을 조용히 파괴한다.**
>
> `monocle login`은 `--env <prod|stg|local>`(기본 `prod`)을 받지만, 저장 경로는
> **환경과 무관하게 `~/.monocle/credentials.json` 하나**다. 환경별 파일이 없다.
> 로그인은 기존 세션 확인도, 확인 절차도 없이 그대로 덮어쓴다
> (`src/commands/login.rs`의 `store.write(&creds)`).
>
> 즉 **프로덕션 세션이 살아있는 상태에서 `monocle login --env stg`를 실행하면 그
> 세션은 경고 없이 사라진다.** 되돌리려면 재인증해야 한다.
>
> **잃는 것이 액세스 토큰만이 아니다.** 프로덕션 리프레시 토큰은 수명이 길어 —
> 관측 시점 기준 **2026-08-22까지 유효** — 그동안 무인(unattended) 프로덕션
> 호출을 가능하게 한다. 덮어쓰면 그 능력이 통째로 사라지고, 재인증에는 사람이
> 붙어야 한다. (날짜는 해당 시점의 실제 세션에서 관측한 값이며 계약이 아니다.)
>
> **환경을 바꾸기 전에 백업할 것:**
> ```bash
> cp ~/.monocle/credentials.json ~/.monocle/credentials.json.prod   # 전환 전
> cp ~/.monocle/credentials.json.prod ~/.monocle/credentials.json   # 복구
> ```
> (백업본도 토큰이므로 `chmod 600`을 유지하고 레포에 넣지 말 것.)
>
> **스테이징은 로그인에 성공해도 동작하지 않을 수 있다.** 스테이징 액세스
> 토큰에는 `email` 클레임이 없어서(stark#1061) jarvice REST가 401을 낼 수 있다.
> 즉 "로그인 성공"을 "스테이징이 정상"으로 읽으면 안 된다 — 프로덕션 세션을
> 날린 대가로 얻은 것이 401뿐일 수 있다.

### 검증 경로 — CLI로 확인한 것은 모바일로 확인한 것이 아니다

`monocle` CLI는 **`X-Session-Id` 헤더를 보내지 않지만 모바일 클라이언트는 보낸다.**
jarvice는 이 헤더로 처리 분기를 가르므로(jarvice `middleware.py:2440-2451`),
**CLI에서 통과한 것이 모바일 경로에서도 통과한다는 보장이 없다.** CLI 결과를
모바일 동작의 근거로 삼는 것은 잘못된 검증이다.

모바일 계열 동작을 검증할 때는 CLI 대신 **헤더 3개를 갖춘 `curl`**을 쓸 것:

```bash
curl -sS https://stg-agent.monocle-ai.com/<endpoint> \
  -H 'content-type: application/json' \
  -H "X-Session-Id: $SESSION_ID" \
  -H "Authorization: Bearer $ACCESS_TOKEN"
```

호스트를 헷갈리지 말 것 — 스테이징 **jarvice** 테넌트는 `stg-agent.monocle-ai.com`
이고, `stg.monocle-ai.com`은 **Stark**(인증 서버)다.

## 설계 원칙 — 조합성·상호운용성 (유닉스 철학)

`monocle`은 **"하나를 잘하고 조합 가능한 도구"** 를 지향한다. 최근 코딩 에이전트가
UI·오케스트레이션을 한 프로세스에 블랙박스로 삼켜 호스트와 충돌하는 것(예: Claude
Code가 자기 TUI 안에서 스크롤/버퍼를 재구현 → 이맥스의 버퍼 관리와 충돌해 이맥스
기능을 못 씀)을 **안티패턴**으로 본다. 우리는 그 반대로 간다.

1. **엔진과 UI를 분리한다 (영구).** CLI는 로직 엔진이다. 화려한 자체 TUI(자체
   스크롤/페인/버퍼 관리)를 넣지 않는다. 인터랙티브 화면이 필요하면 **호스트가
   소유**하고 CLI는 프로토콜로 구동된다 — 그래서 `monocle acp`는 헤드리스이고, UI는
   에디터/데스크탑/Craft(ACP 클라이언트)의 몫이다. 엔진이 UI를 안 가지므로 호스트의
   버퍼/스크롤과 싸우지 않는다.
2. **텍스트 스트림이 인터페이스다.** 답변/데이터 → **stdout**, 진행·로그·제어 안내
   → **stderr**, 상태 → **exit code**. 이 분리는 불변식이다(섞지 말 것). 그래서
   `monocle chat`/`agent`가 파이프로 조합된다. 기계가독이 필요한 곳은 JSON 출력을
   제공한다. (에이전트 루프의 듀얼채널 `ToolOutcome{llm, ui}`도 같은 원칙 — 모델용/
   사람용 스트림 분리.)
3. **흡수(lock-in)보다 접속(interop).** 새 기능은 "우리 안에 다 넣기"보다 "표준으로
   노출해 남이 조합"을 먼저 검토한다. 이미: OpenAI 호환 API(아무 OpenAI SDK), **ACP**
   와이어(아무 ACP 클라이언트), credentials drop-in 계약. 프로토콜을 안정 계약으로
   두고 구현(crate 등)은 교체 가능한 어댑터로 격리한다(예: ACP crate는 `src/acp.rs`
   한 곳).
4. **작게·조합 가능하게.** 서브커맨드는 git/busybox식 툴박스지만, 각각 단독으로 하나를
   잘하고 stdin/stdout으로 조합 가능해야 한다. 거대 오케스트레이터화를 지양한다.
   (유닉스 철학이 늘 우아하진 않다 — 과분해도 비용이니 균형을 잡되, 의심스러우면
   조합·상호운용 쪽으로 기운다.)

## README 반영 규칙
- 커밋마다 사용자 경험(UX)이 바뀌는지 확인하고, 바뀌었다면 README.md도 함께 업데이트할 것
- 내부 리팩토링이나 구현 개선은 README 반영 불필요
- 새 명령어, 옵션 변경, 설정 흐름 변경 등 사용자에게 보이는 변화만 반영 대상

## 에이전트 모드 (WIP — Path B)

`monocle`를 **헤드리스 에이전트**로 확장 중. **코딩 에이전트가 아니라**, monocle
**Craft**(및 데스크탑)가 구동하는 **모델‑무관 서버‑에이전트 백엔드** — Craft의
Sonnet 종속 비용을 Craft 코어 변경 없이 완화(= 모델 자유도 G1)하는 게 목적.

- **설계(SDD/에픽):** warmblood-kr/monocle#158 · **구현 상태:** warmblood-kr/monocle-cli#44
- **코드:** `src/agent/`(`providers`=LLM 추상화/G1, `tools`=read/write/edit +
  크로스플랫폼 shell, `runner`=agent-core 루프[동기], `session`=JSONL 세션) +
  `src/commands/agent.rs`(`monocle agent`, REPL/스트리밍/`--session`) +
  **`src/acp.rs`(`monocle acp` — ACP stdio 서버; async는 여기에만 격리, 코어는 동기)**.
  모든 HTTP는 `src/net.rs` 파사드.
- **작업 브랜치:** `feature/agent-mode-acp` (base `rust-rewrite`; providers/agent는
  PR #45로 `rust-rewrite`에 머지됨). push/PR은 승인 게이트.
- **시퀀스(§9):** Phase 0(얼개)✅ → Phase 1 스트리밍+멀티턴✅ → Phase 2 세션/
  듀얼채널✅ → **Phase 3 ACP 표면**✅(구동/스트리밍/권한 위임/ToolCall 라이프사이클/
  미로그인 생존/세션별 모델[`_meta.monocle.model`]) → Phase 4 정교화(client fs/
  terminal 콜백 등 남음).
- pi(earendil-works/pi, MIT)를 설계 참고로만(코드 차용 X); ACP = Zed Agent Client
  Protocol, crate `agent-client-protocol` 0.9(교체 가능 어댑터, `acp.rs`에 격리).

## 지식 관리 — wb-para 스킬 적극 사용

이 저장소는 모노클 AI 코드베이스입니다. 작업 중 나오는 결정·통찰·운영 지식은 `warmble-jumble` vault에 축적해야 팀이 재활용할 수 있으므로, wb-para sub-skill을 능동적으로 호출하세요.

- **작업 시작 시 관련 컨텍스트 탐색** → `wb-para:find`
- **아키텍처 결정·비자명한 패턴 발견·PR 리뷰 통찰** → `wb-para:capture`
- **장애 대응·VOC 처리 후 runbook/postmortem** → `wb-para:capture`
- **"전에 비슷한 거 본 적 있는데" 신호** → `wb-para:find`를 먼저 실행
- **하나의 노트가 여러 개념을 섞기 시작** → `wb-para:link`로 atomic 분리

vault 노트가 코드 작업의 출발점·종착점이 되도록 유지하세요.
