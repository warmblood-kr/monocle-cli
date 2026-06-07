# Monocle CLI

## Stack
- TypeScript, Node.js 18+
- Commander.js (CLI framework)
- Vitest (testing)

## Commands
```bash
npm run build    # TypeScript 컴파일
npm run test     # 테스트 실행
npm run lint     # 타입 체크 (noEmit)
```

## Conventions
- 커밋 메시지, PR, 이슈: 한국어
- 브랜치 전략: `feature branch → devel → staging → main`
- 버전: package.json + src/cli.ts 두 곳 동시 업데이트

## README 반영 규칙
- 커밋마다 사용자 경험(UX)이 바뀌는지 확인하고, 바뀌었다면 README.md도 함께 업데이트할 것
- 내부 리팩토링이나 구현 개선은 README 반영 불필요
- 새 명령어, 옵션 변경, 설정 흐름 변경 등 사용자에게 보이는 변화만 반영 대상

## 지식 관리 — wb-para 스킬 적극 사용

이 저장소는 모노클 AI 코드베이스입니다. 작업 중 나오는 결정·통찰·운영 지식은 `warmble-jumble` vault에 축적해야 팀이 재활용할 수 있으므로, wb-para sub-skill을 능동적으로 호출하세요.

- **작업 시작 시 관련 컨텍스트 탐색** → `wb-para:find`
- **아키텍처 결정·비자명한 패턴 발견·PR 리뷰 통찰** → `wb-para:capture`
- **장애 대응·VOC 처리 후 runbook/postmortem** → `wb-para:capture`
- **"전에 비슷한 거 본 적 있는데" 신호** → `wb-para:find`를 먼저 실행
- **하나의 노트가 여러 개념을 섞기 시작** → `wb-para:link`로 atomic 분리

vault 노트가 코드 작업의 출발점·종착점이 되도록 유지하세요.
