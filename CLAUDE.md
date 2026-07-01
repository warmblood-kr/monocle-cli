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
