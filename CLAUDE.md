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
