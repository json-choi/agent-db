# ADR 0003: Terminal session과 capability

- 상태: 승인
- 날짜: 2026-07-24
- 관련 계획: Phase 3~5

## 결정

첫 CLI DB 접근은 DopeDB가 만든 인앱 Terminal에서만 허용한다. session은 생성 시
workspace와 connection에 pin하며 Workbench connection이 바뀌어도 retarget하지 않는다.

Runtime은 session마다 다음을 memory에만 보관한다.

- terminal session id
- runtime id
- account/workspace scope
- pinned connection id와 revision
- actor/Agent profile
- capability set
- 256-bit random opaque token
- expiry와 rotation

PTY child에는 필요한 값만 환경으로 전달한다.

```text
DOPEDB_RUNTIME_FILE
DOPEDB_TERMINAL_SESSION_ID
DOPEDB_CONNECTION_SCOPE
DOPEDB_SESSION_TOKEN
```

token은 DB credential이 아니다. argv, shell profile, runtime discovery, CLI JSON, audit,
terminal replay에 기록하지 않는다.

## Revocation

다음 사건에서 DB command보다 먼저 즉시 revoke한다.

- Terminal 종료/restart
- workspace/account 전환
- membership/grant revoke
- connection update/delete
- provider lease rotation/expiry
- app 종료

이미 만들어진 plan은 runtime, owner session, workspace/account, connection revision을 모두
확인하므로 다른 Terminal에서 사용할 수 없다.

## 경계

이 capability는 같은 OS user 권한으로 실행되는 악성 process를 완전히 막는 sandbox라고
주장하지 않는다. 목적은 Agent 오작동, scope 혼선, 다른 Terminal의 plan 재사용을
차단하는 것이다.

외부 shell DB 접근은 후속 `dopedb session start`에서 앱의 명시적 승인 후 CLI가
단기 token을 가진 child shell/Agent를 직접 시작하는 방식으로만 추가한다.
