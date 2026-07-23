# Catalog V2 contract

정본 serde 계약은 `dopedb-protocol/src/catalog.rs`다. Catalog V2의
`schemaVersion`은 `2`로 시작하며 다음 소비자가 동일 snapshot을 사용한다.

- CLI catalog/schema/table command
- SQL completion과 hover
- Table data/structure editor
- DDL IR validation과 stale proposal 검사
- ERD/UML graph와 layout reconciliation
- Import mapping과 shared resource provenance

## 불변 조건

- object ordering을 canonicalize한 뒤 구조 metadata의 SHA-256 fingerprint를 만든다.
- 동일 metadata는 수집 순서와 관계없이 동일 fingerprint를 가져야 한다.
- wire/cache 역직렬화 시 fingerprint 형식뿐 아니라 canonical metadata의 실제
  SHA-256과 일치하는지도 검증하고, 불일치하면 fail closed한다.
- connection id, database name, capture time, row estimate는 identity/display
  metadata이므로 schema fingerprint에서 제외한다. engine과 object/column/constraint/
  index metadata는 포함한다.
- engine-native id는 안전하고 안정적인 경우에만 `nativeId`로 노출한다.
- cache schema version이 다르면 best-effort deserialize하지 않고 lazy refresh한다.
- connection/driver/provider lease revision이 바뀌면 snapshot을 stale로 본다.
- secret, hostname, username, connection URL은 Catalog에 포함하지 않는다.
- MongoDB field는 bounded sample의 관찰값이며 보장된 schema라고 표시하지 않는다.

## 최소 relation metadata

- catalog/namespace/name/kind object reference
- columns: ordinal/native type/type family/size/null/default/generated/identity/collation/comment
- constraints: PK/unique/FK/check와 action/validation state
- indexes: method/key expression/direction/include/predicate/unique/validity
- partition parent/children
- comment와 row estimate

정본 DTO, canonical fingerprint, scoped cache는 이 계약의 V2 fixture를 기준으로
검증한다. 엔진별 metadata 확장과 완전한 fixture는 `CAT-02/03`에서 계속 추가한다.
