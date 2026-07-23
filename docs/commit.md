# Commit Message Guide


## 언어 규칙


모든 커밋 메시지는 한국어로 작성한다.


허용:


fix(payment): 정산 금액 중복 계산 수정


feat(auth): 구글 로그인 추가


test(payment-domain): JUnit Platform 활성화


금지:


fix(payment): fix duplicate settlement bug


feat(auth): add google oauth login


## 형식


Conventional Commits v1.0.0 형식을 사용한다.


기본 구조


<type>(<scope>): <description>


<blank line>


- 변경 사항 1
- 변경 사항 2
- 변경 사항 3


<blank line>


Refs: #123


또는


Closes: #123


관련 이슈가 여러 개인 경우


Refs: #123, #456


## 타입


- feat
- fix
- test
- refactor
- build
- ci
- docs
- chore


## 제목 규칙


- description은 한국어로 작성한다.
- 명사형보다 동사형을 선호한다.
- 현재 변경 사항만 설명한다.
- 50자 이내를 권장한다.


예시


fix(payment): 정산 금액 중복 계산 수정


## 본문 규칙


본문은 변경 사항을 요약하는 용도로 작성한다.


- 반드시 목록(Bullet List) 형태로 작성한다.
- 변경한 기능, 로직, 테스트 등을 중심으로 작성한다.
- 구현 세부 코드나 분석 내용은 작성하지 않는다.
- Why가 필요한 경우에만 간단히 추가한다.
- 목록은 2~8개 정도를 권장한다.


예시


- 정산 금액 계산 중복 로직 제거
- Redis 캐시 갱신 순서 수정
- 관련 단위 테스트 추가


## 이슈 링크 규칙


저장소 소유자가 사용자 요청 작업을 `main`에 직접 커밋할 때는 커밋 전에 기존 GitHub Issue를 사용하거나 새 이슈를 발행해야 한다. 커밋 마지막에는 해당 이슈를 반드시 추가한다.


기여자 작업도 관련 GitHub Issue가 존재하면 커밋 마지막에 반드시 추가한다.


형식


Refs: #123


또는


Closes: #123


여러 개인 경우


Refs: #123, #456


저장소 소유자의 사용자 요청 작업은 이슈를 생략할 수 없다. 기여자 작업은 관련 이슈가 없다면 생략할 수 있다.


## 금지


다음 내용은 커밋 메시지에 포함하지 않는다.


- Confidence
- Scope-risk
- Rejected
- Tested
- Not-tested
- Analysis
- AI 생성 설명
- 장문의 변경 보고서
