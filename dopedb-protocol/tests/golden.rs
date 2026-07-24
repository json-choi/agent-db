use dopedb_protocol::{
    catalog::CatalogSnapshot, decode_arguments, AppOpenCommand, AppOpenResult,
    AuthenticationRequirement, CatalogShowCommand, CommandName, CommandSpec, ConnectionListCommand,
    ConnectionShowCommand, ConnectionTestCommand, ErrorCode, OperationCancelCommand,
    OperationShowCommand, OperationWaitCommand, ProtocolError, QueryCancelCommand,
    QueryPlanCommand, QueryRunCommand, RequestEnvelope, ResponseEnvelope, RuntimeDiscovery,
    SchemaListCommand, SqlProposeCommand, StatusCommand, StatusResult, TableDescribeCommand,
    VersionCommand, VersionResult, COMMAND_SCHEMA_VERSION, PROTOCOL_MAX,
};
use serde::Deserialize;
use serde_json::{json, Value};

fn value(source: &str) -> Value {
    serde_json::from_str(source).expect("fixture must be valid JSON")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CliCommandContract {
    command: CommandName,
    arguments: Value,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    result_fixture: Option<String>,
}

fn typed_cli_contract<C: CommandSpec>(request: &RequestEnvelope, result: &Value) {
    assert_eq!(
        C::AUTHENTICATION,
        AuthenticationRequirement::TerminalSession
    );
    let arguments = decode_arguments::<C>(request).expect("typed command arguments");
    assert_eq!(serde_json::to_value(arguments).unwrap(), request.arguments);
    let typed_result: C::Result =
        serde_json::from_value(result.clone()).expect("typed command result");
    assert_eq!(serde_json::to_value(typed_result).unwrap(), *result);
}

fn operation_summary_fixture() -> Value {
    json!({
        "operationId": "00000000-0000-0000-0000-000000000003",
        "connectionId": "00000000-0000-0000-0000-000000000001",
        "kind": "write_sql",
        "state": "pending_approval",
        "riskLevel": "medium",
        "payloadHash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "expiresAt": "2026-07-24T00:05:00Z",
        "createdAt": "2026-07-24T00:00:00Z",
        "updatedAt": "2026-07-24T00:00:00Z"
    })
}

fn resolve_result_fixture(contract: &CliCommandContract) -> Value {
    if let Some(result) = &contract.result {
        return result.clone();
    }
    match contract.result_fixture.as_deref() {
        Some("catalog-snapshot-v2") => value(include_str!("fixtures/catalog-snapshot-v2.json")),
        Some("catalog-relation-0") => {
            let catalog = value(include_str!("fixtures/catalog-snapshot-v2.json"));
            json!({
                "connectionId": catalog["connectionId"],
                "relation": catalog["relations"][0]
            })
        }
        Some("operation-summary") => operation_summary_fixture(),
        fixture => panic!("unknown result fixture {fixture:?}"),
    }
}

fn assert_cli_command_types(command: CommandName, request: &RequestEnvelope, result: &Value) {
    match command {
        CommandName::ConnectionList => typed_cli_contract::<ConnectionListCommand>(request, result),
        CommandName::ConnectionShow => typed_cli_contract::<ConnectionShowCommand>(request, result),
        CommandName::ConnectionTest => typed_cli_contract::<ConnectionTestCommand>(request, result),
        CommandName::CatalogShow => typed_cli_contract::<CatalogShowCommand>(request, result),
        CommandName::SchemaList => typed_cli_contract::<SchemaListCommand>(request, result),
        CommandName::TableDescribe => typed_cli_contract::<TableDescribeCommand>(request, result),
        CommandName::QueryPlan => typed_cli_contract::<QueryPlanCommand>(request, result),
        CommandName::QueryRun => typed_cli_contract::<QueryRunCommand>(request, result),
        CommandName::QueryCancel => typed_cli_contract::<QueryCancelCommand>(request, result),
        CommandName::SqlPropose => typed_cli_contract::<SqlProposeCommand>(request, result),
        CommandName::OperationShow => typed_cli_contract::<OperationShowCommand>(request, result),
        CommandName::OperationWait => typed_cli_contract::<OperationWaitCommand>(request, result),
        CommandName::OperationCancel => {
            typed_cli_contract::<OperationCancelCommand>(request, result)
        }
        unsupported => panic!("manifest contains unsupported command {unsupported}"),
    }
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
fn every_phase_three_cli_command_has_request_success_error_and_redaction_goldens() {
    let contracts: Vec<CliCommandContract> =
        serde_json::from_str(include_str!("fixtures/cli-command-contract-v1.json"))
            .expect("CLI command manifest must decode");
    let expected = [
        CommandName::ConnectionList,
        CommandName::ConnectionShow,
        CommandName::ConnectionTest,
        CommandName::CatalogShow,
        CommandName::SchemaList,
        CommandName::TableDescribe,
        CommandName::QueryPlan,
        CommandName::QueryRun,
        CommandName::QueryCancel,
        CommandName::SqlPropose,
        CommandName::OperationShow,
        CommandName::OperationWait,
        CommandName::OperationCancel,
    ];
    assert_eq!(
        contracts
            .iter()
            .map(|contract| contract.command)
            .collect::<Vec<_>>(),
        expected
    );

    for (index, contract) in contracts.iter().enumerate() {
        let request_id =
            uuid::Uuid::from_u128(0x018f_1111_2222_7333_8444_5555_0000_0000 + index as u128);
        let request_value = json!({
            "protocolVersion": PROTOCOL_MAX,
            "commandSchemaVersion": COMMAND_SCHEMA_VERSION,
            "requestId": request_id,
            "authentication": {
                "terminalSessionId": "018faaaa-bbbb-7ccc-8ddd-eeeeeeeeeeee",
                "token": "fixture-only-session-capability"
            },
            "command": contract.command,
            "arguments": contract.arguments
        });
        let request: RequestEnvelope =
            serde_json::from_value(request_value.clone()).expect("golden request envelope");
        assert_eq!(serde_json::to_value(&request).unwrap(), request_value);

        let result = resolve_result_fixture(contract);
        assert_cli_command_types(contract.command, &request, &result);
        let success = ResponseEnvelope::success(PROTOCOL_MAX, request_id, result);
        let success_value = serde_json::to_value(&success).unwrap();
        let success_round_trip: ResponseEnvelope =
            serde_json::from_value(success_value.clone()).expect("golden success envelope");
        assert!(success_round_trip.is_ok());
        assert_eq!(
            serde_json::to_value(&success_round_trip).unwrap(),
            success_value
        );

        let error = ResponseEnvelope::failure(
            PROTOCOL_MAX,
            request_id,
            ProtocolError::new(ErrorCode::ScopeDenied, false),
        );
        let error_value = serde_json::to_value(&error).unwrap();
        let error_round_trip: ResponseEnvelope =
            serde_json::from_value(error_value.clone()).expect("golden error envelope");
        assert_eq!(
            error_round_trip.error().map(ProtocolError::code),
            Some(ErrorCode::ScopeDenied)
        );
        assert_eq!(
            serde_json::to_value(&error_round_trip).unwrap(),
            error_value
        );

        let request_debug = format!("{request:?}");
        let success_debug = format!("{success:?}");
        assert!(!request_debug.contains("fixture-only-session-capability"));
        assert!(!request_debug.contains("SELECT id"));
        assert!(!success_debug.contains("reader@example.test"));
        assert!(!success_debug.contains("aaaaaaaaaaaaaaaa"));

        let safe_response_snapshot = success_value.to_string().to_ascii_lowercase();
        for forbidden in [
            "fixture-only-session-capability",
            "postgresql://",
            "\"password\"",
            "\"credential\"",
            "\"token\"",
        ] {
            assert!(
                !safe_response_snapshot.contains(forbidden),
                "{} success snapshot contains forbidden material {forbidden}",
                contract.command
            );
        }
    }
}

#[test]
fn active_app_commands_match_the_v1_golden_contract() {
    let version_request_source = include_str!("fixtures/version-request.json");
    let version_request: RequestEnvelope =
        serde_json::from_str(version_request_source).expect("version request must decode");
    decode_arguments::<VersionCommand>(&version_request).expect("typed version request");
    assert_eq!(
        serde_json::to_value(&version_request).unwrap(),
        value(version_request_source)
    );

    let status_request_source = include_str!("fixtures/status-request.json");
    let status_request: RequestEnvelope =
        serde_json::from_str(status_request_source).expect("status request must decode");
    decode_arguments::<StatusCommand>(&status_request).expect("typed status request");
    assert_eq!(
        serde_json::to_value(&status_request).unwrap(),
        value(status_request_source)
    );

    let app_open_request_source = include_str!("fixtures/app-open-request.json");
    let app_open_request: RequestEnvelope =
        serde_json::from_str(app_open_request_source).expect("app open request must decode");
    decode_arguments::<AppOpenCommand>(&app_open_request).expect("typed app open request");
    assert_eq!(
        serde_json::to_value(&app_open_request).unwrap(),
        value(app_open_request_source)
    );

    let version_success_source = include_str!("fixtures/version-success.json");
    let version_success: ResponseEnvelope =
        serde_json::from_str(version_success_source).expect("version response must decode");
    let _: VersionResult =
        serde_json::from_value(version_success.result().cloned().unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&version_success).unwrap(),
        value(version_success_source)
    );

    let status_success_source = include_str!("fixtures/status-success.json");
    let status_success: ResponseEnvelope =
        serde_json::from_str(status_success_source).expect("status response must decode");
    let _: StatusResult =
        serde_json::from_value(status_success.result().cloned().unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&status_success).unwrap(),
        value(status_success_source)
    );

    let app_open_success_source = include_str!("fixtures/app-open-success.json");
    let app_open_success: ResponseEnvelope =
        serde_json::from_str(app_open_success_source).expect("app open response must decode");
    let _: AppOpenResult =
        serde_json::from_value(app_open_success.result().cloned().unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&app_open_success).unwrap(),
        value(app_open_success_source)
    );

    for source in [
        include_str!("fixtures/app-open-error.json"),
        include_str!("fixtures/version-error.json"),
        include_str!("fixtures/status-error.json"),
        include_str!("fixtures/policy-blocked.json"),
    ] {
        let response: ResponseEnvelope =
            serde_json::from_str(source).expect("response fixture must decode");
        response.validate().expect("response invariant");
        assert_eq!(serde_json::to_value(&response).unwrap(), value(source));
    }
}

#[test]
fn runtime_discovery_matches_the_secret_free_v1_contract() {
    let source = include_str!("fixtures/runtime-discovery.json");
    let discovery: RuntimeDiscovery =
        serde_json::from_str(source).expect("runtime discovery must decode");
    discovery.validate().expect("runtime discovery invariant");
    assert_eq!(serde_json::to_value(&discovery).unwrap(), value(source));

    let serialized = serde_json::to_string(&discovery).unwrap();
    for forbidden in [
        "token",
        "password",
        "credential",
        "database",
        "workspace",
        "connection",
    ] {
        assert!(!serialized.to_ascii_lowercase().contains(forbidden));
    }
}

#[test]
fn unknown_envelope_and_active_command_fields_fail_closed() {
    let mut fixture = value(include_str!("fixtures/query-plan-request.json"));
    fixture
        .as_object_mut()
        .unwrap()
        .insert("approved".into(), Value::Bool(true));
    assert!(serde_json::from_value::<RequestEnvelope>(fixture).is_err());

    let mut status = value(include_str!("fixtures/status-request.json"));
    status["arguments"]["approved"] = Value::Bool(true);
    let request: RequestEnvelope = serde_json::from_value(status).unwrap();
    assert!(decode_arguments::<StatusCommand>(&request).is_err());

    let future: RequestEnvelope = serde_json::from_value(serde_json::json!({
        "protocolVersion": 1,
        "commandSchemaVersion": 2,
        "requestId": "018f1111-2222-7333-8444-555566667777",
        "command": "future.command",
        "arguments": {"approved": true}
    }))
    .unwrap();
    assert_eq!(future.command, dopedb_protocol::CommandName::Unknown);
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
