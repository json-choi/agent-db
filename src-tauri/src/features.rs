//! Runtime feature-flag vocabulary for the staged CLI/Terminal platform rollout.
//! Every new execution surface starts disabled until its migration and rollback
//! behavior are proven.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureFlag {
    OperationRuntimeV1,
    LocalBrokerV1,
    CliV1,
    SkillManagerV1,
    TerminalDockV1,
    McpDeprecated,
    CatalogV2,
    DdlIrV1,
    SqlDocumentsV1,
    TableChangesV1,
    ErdV1,
    JobsV1,
    PluginsV1,
    WorkspaceResourcesV1,
    RealtimeCollaborationV1,
}

impl FeatureFlag {
    pub const ALL: [Self; 15] = [
        Self::OperationRuntimeV1,
        Self::LocalBrokerV1,
        Self::CliV1,
        Self::SkillManagerV1,
        Self::TerminalDockV1,
        Self::McpDeprecated,
        Self::CatalogV2,
        Self::DdlIrV1,
        Self::SqlDocumentsV1,
        Self::TableChangesV1,
        Self::ErdV1,
        Self::JobsV1,
        Self::PluginsV1,
        Self::WorkspaceResourcesV1,
        Self::RealtimeCollaborationV1,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OperationRuntimeV1 => "operation_runtime_v1",
            Self::LocalBrokerV1 => "local_broker_v1",
            Self::CliV1 => "cli_v1",
            Self::SkillManagerV1 => "skill_manager_v1",
            Self::TerminalDockV1 => "terminal_dock_v1",
            Self::McpDeprecated => "mcp_deprecated",
            Self::CatalogV2 => "catalog_v2",
            Self::DdlIrV1 => "ddl_ir_v1",
            Self::SqlDocumentsV1 => "sql_documents_v1",
            Self::TableChangesV1 => "table_changes_v1",
            Self::ErdV1 => "erd_v1",
            Self::JobsV1 => "jobs_v1",
            Self::PluginsV1 => "plugins_v1",
            Self::WorkspaceResourcesV1 => "workspace_resources_v1",
            Self::RealtimeCollaborationV1 => "realtime_collaboration_v1",
        }
    }
}

/// Immutable process snapshot. A later settings service may build a new snapshot,
/// but adapters cannot toggle safety-sensitive flags by passing request fields.
#[derive(Debug, Clone, Default)]
pub struct FeatureFlags {
    enabled: BTreeSet<FeatureFlag>,
}

impl FeatureFlags {
    pub fn new(enabled: impl IntoIterator<Item = FeatureFlag>) -> Self {
        Self {
            enabled: enabled.into_iter().collect(),
        }
    }

    pub fn is_enabled(&self, flag: FeatureFlag) -> bool {
        self.enabled.contains(&flag)
    }

    pub fn enabled_names(&self) -> Vec<&'static str> {
        self.enabled.iter().map(|flag| flag.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_new_platform_feature_is_off_by_default() {
        let flags = FeatureFlags::default();
        assert!(FeatureFlag::ALL
            .into_iter()
            .all(|flag| !flags.is_enabled(flag)));
        assert!(flags.enabled_names().is_empty());
    }

    #[test]
    fn explicit_snapshot_only_enables_named_features() {
        let flags = FeatureFlags::new([FeatureFlag::CatalogV2, FeatureFlag::DdlIrV1]);
        assert!(flags.is_enabled(FeatureFlag::CatalogV2));
        assert!(flags.is_enabled(FeatureFlag::DdlIrV1));
        assert!(!flags.is_enabled(FeatureFlag::OperationRuntimeV1));
    }

    #[test]
    fn serialized_names_match_the_v1_golden_fixture() {
        let actual = serde_json::to_value(FeatureFlag::ALL).unwrap();
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/platform-feature-flags-v1.json"
        ))
        .unwrap();
        assert_eq!(actual, expected);
    }
}
