//! Workspace-local authorization primitives. This module is provider-neutral: it
//! intersects membership, grants, environment policy, and external capability without
//! treating any one of those layers as interchangeable authority.

use crate::model::{AuthorizationDecision, AuthorizationRequest, WorkspaceRole};

/// Evaluate a workspace action using fail-closed, narrowing authorization layers.
/// Explicit deny always wins; production and external capability checks cannot be
/// bypassed by an Owner role or an explicit workspace grant.
pub fn authorize(request: &AuthorizationRequest) -> AuthorizationDecision {
    let deny = |reason: &str| AuthorizationDecision {
        allowed: false,
        reason: reason.to_string(),
        approval_required: false,
    };

    if !request.actor.active {
        return deny("workspace membership is not active");
    }
    if request.actor.workspace_id != request.resource.workspace_id {
        return deny("resource belongs to another workspace");
    }
    if request.explicitly_denied {
        return deny("an explicit resource deny applies");
    }
    if !request.external_capability_available {
        return deny("the external provider or database did not grant this capability");
    }
    if request.resource.environment.as_deref() == Some("prod") && !request.production_access {
        return deny("production access is required");
    }
    if !request.explicitly_granted && !role_allows(request.actor.role, &request.action) {
        return deny("workspace role does not grant this action");
    }

    AuthorizationDecision {
        allowed: true,
        reason: if request.approval_required {
            "authorized after approval".to_string()
        } else {
            "authorized".to_string()
        },
        approval_required: request.approval_required,
    }
}

fn role_allows(role: WorkspaceRole, action: &str) -> bool {
    let required = if matches!(action, "workspace.delete" | "workspace.transfer") {
        WorkspaceRole::Owner
    } else if action.starts_with("workspace.member.")
        || action.starts_with("connection.manage")
        || action.starts_with("provider.integration.manage")
    {
        WorkspaceRole::Admin
    } else if action.starts_with("resource.edit")
        || action.starts_with("dashboard.edit")
        || action.starts_with("report.publish")
    {
        WorkspaceRole::Editor
    } else if action.starts_with("connection.execute") || action.starts_with("report.create") {
        WorkspaceRole::Analyst
    } else {
        WorkspaceRole::Viewer
    };
    role >= required
}

#[cfg(test)]
mod tests {
    use super::authorize;
    use crate::model::{
        AuthorizationActor, AuthorizationRequest, AuthorizationResource, WorkspaceRole,
    };
    use uuid::Uuid;

    fn request(role: WorkspaceRole) -> AuthorizationRequest {
        let workspace_id = Uuid::new_v4();
        AuthorizationRequest {
            actor: AuthorizationActor {
                member_id: Uuid::new_v4(),
                workspace_id,
                role,
                active: true,
            },
            action: "connection.execute_read".into(),
            resource: AuthorizationResource {
                workspace_id,
                kind: "connection".into(),
                id: Uuid::new_v4(),
                parent_id: None,
                environment: Some("dev".into()),
            },
            explicitly_granted: false,
            explicitly_denied: false,
            external_capability_available: true,
            production_access: false,
            approval_required: false,
        }
    }

    #[test]
    fn explicit_deny_wins_over_owner_and_grant() {
        let mut input = request(WorkspaceRole::Owner);
        input.explicitly_granted = true;
        input.explicitly_denied = true;
        assert!(!authorize(&input).allowed);
    }

    #[test]
    fn production_and_external_authority_remain_narrowing_layers() {
        let mut input = request(WorkspaceRole::Owner);
        input.resource.environment = Some("prod".into());
        assert!(!authorize(&input).allowed);

        input.production_access = true;
        input.external_capability_available = false;
        assert!(!authorize(&input).allowed);
    }

    #[test]
    fn analyst_can_read_but_viewer_needs_an_explicit_grant() {
        assert!(authorize(&request(WorkspaceRole::Analyst)).allowed);
        let mut viewer = request(WorkspaceRole::Viewer);
        assert!(!authorize(&viewer).allowed);
        viewer.explicitly_granted = true;
        assert!(authorize(&viewer).allowed);
    }
}
