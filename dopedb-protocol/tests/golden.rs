use dopedb_protocol::{
    catalog::CatalogSnapshot, RequestEnvelope, ResponseEnvelope, COMMAND_SCHEMA_VERSION,
    PROTOCOL_MAX,
};
use serde_json::Value;

fn value(source: &str) -> Value {
    serde_json::from_str(source).expect("fixture must be valid JSON")
}

#[test]
fn query_plan_request_matches_v1_golden_contract() {
    let source = include_str!("fixtures/query-plan-request.json");
    let request: RequestEnvelope =
        serde_json::from_str(source).expect("request fixture must decode");

    assert_eq!(request.protocol_version, PROTOCOL_MAX);
    assert_eq!(request.command_schema_version, COMMAND_SCHEMA_VERSION);
    assert_eq!(request.command.as_str(), "query.plan");
    assert_eq!(serde_json::to_value(&request).unwrap(), value(source));

    let debug = format!("{request:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("fixture-only-session-capability"));
}

#[test]
fn success_and_error_responses_match_v1_golden_contract() {
    for source in [
        include_str!("fixtures/status-success.json"),
        include_str!("fixtures/policy-blocked.json"),
    ] {
        let response: ResponseEnvelope =
            serde_json::from_str(source).expect("response fixture must decode");
        response.validate().expect("response invariant");
        assert_eq!(serde_json::to_value(&response).unwrap(), value(source));
    }
}

#[test]
fn unknown_envelope_fields_fail_closed() {
    let mut fixture = value(include_str!("fixtures/query-plan-request.json"));
    fixture
        .as_object_mut()
        .unwrap()
        .insert("approved".into(), Value::Bool(true));
    assert!(serde_json::from_value::<RequestEnvelope>(fixture).is_err());
}

#[test]
fn command_names_match_the_v1_catalog() {
    let actual = dopedb_protocol::CommandName::ALL
        .into_iter()
        .map(|command| command.as_str())
        .collect::<Vec<_>>();
    let expected: Vec<String> =
        serde_json::from_str(include_str!("fixtures/command-catalog-v1.json")).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn catalog_snapshot_matches_the_v2_golden_contract() {
    let source = include_str!("fixtures/catalog-snapshot-v2.json");
    let snapshot: CatalogSnapshot =
        serde_json::from_str(source).expect("Catalog V2 fixture must decode");

    assert_eq!(snapshot.schema_version(), 2);
    assert!(snapshot.has_canonical_fingerprint());
    assert_eq!(serde_json::to_value(&snapshot).unwrap(), value(source));

    let mut structural_tamper = value(source);
    structural_tamper["relations"][0]["columns"][0]["nativeType"] = Value::from("text");
    assert!(serde_json::from_value::<CatalogSnapshot>(structural_tamper).is_err());

    let mut non_structural_change = value(source);
    non_structural_change["database"] = Value::from("production");
    non_structural_change["relations"][0]["rowEstimate"] = Value::from(43);
    let changed: CatalogSnapshot =
        serde_json::from_value(non_structural_change).expect("display metadata is not schema");
    assert_eq!(changed.fingerprint(), snapshot.fingerprint());
}
