// Durable authorization-mutation gates. A UUID owns each claim; timestamps are
// used only to recover abandoned claims and never as compare-and-swap tokens.
import "server-only";

import { randomUUID } from "node:crypto";
import { sql } from "drizzle-orm";
import { db } from "./db";
import {
  member,
  workspaceConnection,
  workspaceCredentialLease,
  workspaceProviderIntegration,
} from "./schema";
import {
  isWorkspaceRole,
  type WorkspaceRoleName,
} from "./workspace-permissions";
import type {
  ManagedAccessMode,
  ManagedProviderLease,
} from "./providers/provider-types";

const REVOCATION_CLAIM_STALE_MS = 5 * 60 * 1_000;
const PENDING_LEASE_SECONDS = 2 * 60;

type MemberGateTarget = {
  kind: "member";
  organizationId: string;
  memberId: string;
  userId: string;
};

type ConnectionGateTarget = {
  kind: "connection";
  organizationId: string;
  connectionId: string;
};

type IntegrationGateTarget = {
  kind: "integration";
  organizationId: string;
  integrationId: string;
};

export type RevocationGateTarget =
  | MemberGateTarget
  | ConnectionGateTarget
  | IntegrationGateTarget;

export type RevocationGateClaim = RevocationGateTarget & {
  claimId: string;
  claimedAt: Date;
  pendingAt: Date;
  firstPending: boolean;
  connectionRevision?: number;
  memberRole?: WorkspaceRoleName;
};

type ClaimedRow = {
  pendingAt: Date | string;
  connectionRevision?: number | string;
  memberRole?: string;
};

export function revocationGateLockKey(target: RevocationGateTarget) {
  switch (target.kind) {
    case "member":
      return `member:${target.organizationId}:${target.userId}`;
    case "connection":
      return `connection:${target.organizationId}:${target.connectionId}`;
    case "integration":
      return `integration:${target.organizationId}:${target.integrationId}`;
  }
}

function parsedClaim(
  target: RevocationGateTarget,
  claimId: string,
  claimedAt: Date,
  row: ClaimedRow | undefined,
): RevocationGateClaim | null {
  if (!row) return null;
  const pendingAt = row.pendingAt instanceof Date
    ? row.pendingAt
    : new Date(row.pendingAt);
  const revision = row.connectionRevision == null
    ? undefined
    : Number(row.connectionRevision);
  const memberRole = row.memberRole;
  if (
    Number.isNaN(pendingAt.valueOf())
    || (revision !== undefined && !Number.isSafeInteger(revision))
    || (memberRole !== undefined && !isWorkspaceRole(memberRole))
  ) {
    throw new Error("Invalid revocation gate claim");
  }
  return {
    ...target,
    claimId,
    claimedAt,
    pendingAt,
    firstPending: pendingAt.valueOf() === claimedAt.valueOf(),
    ...(revision === undefined ? {} : { connectionRevision: revision }),
    ...(memberRole === undefined ? {} : { memberRole }),
  };
}

export async function claimRevocationGate(
  target: RevocationGateTarget,
): Promise<RevocationGateClaim | null> {
  const claimedAt = new Date();
  const staleBefore = new Date(claimedAt.valueOf() - REVOCATION_CLAIM_STALE_MS);
  const claimId = randomUUID();
  let rows: ClaimedRow[];

  if (target.kind === "member") {
    const result = await db.execute<ClaimedRow>(sql`
      WITH gate_lock AS (
        SELECT pg_advisory_xact_lock(hashtextextended(${revocationGateLockKey(target)}, 0))
      ),
      claimed AS (
        UPDATE ${member} AS target
        SET "revocation_pending_at" =
              COALESCE(target."revocation_pending_at", ${claimedAt}),
            "revocation_claimed_at" = ${claimedAt},
            "revocation_claim_id" = ${claimId}::uuid
        FROM gate_lock
        WHERE target."id" = ${target.memberId}
          AND target."organization_id" = ${target.organizationId}
          AND target."user_id" = ${target.userId}
          AND (
            target."revocation_claim_id" IS NULL
            OR target."revocation_claimed_at" < ${staleBefore}
          )
        RETURNING target."revocation_pending_at" AS "pendingAt",
                  target."role" AS "memberRole"
      )
      SELECT * FROM claimed
    `);
    rows = result.rows;
  } else if (target.kind === "connection") {
    const result = await db.execute<ClaimedRow>(sql`
      WITH gate_lock AS (
        SELECT pg_advisory_xact_lock(hashtextextended(${revocationGateLockKey(target)}, 0))
      ),
      claimed AS (
        UPDATE ${workspaceConnection} AS target
        SET "revision" = CASE
              WHEN target."revocation_pending_at" IS NULL
                THEN target."revision" + 1
              ELSE target."revision"
            END,
            "revocation_pending_at" =
              COALESCE(target."revocation_pending_at", ${claimedAt}),
            "revocation_claimed_at" = ${claimedAt},
            "revocation_claim_id" = ${claimId}::uuid
        FROM gate_lock
        WHERE target."id" = ${target.connectionId}::uuid
          AND target."organization_id" = ${target.organizationId}
          AND target."deleted_at" IS NULL
          AND (
            target."revocation_claim_id" IS NULL
            OR target."revocation_claimed_at" < ${staleBefore}
          )
        RETURNING target."revocation_pending_at" AS "pendingAt",
                  target."revision" AS "connectionRevision"
      )
      SELECT * FROM claimed
    `);
    rows = result.rows;
  } else {
    const result = await db.execute<ClaimedRow>(sql`
      WITH gate_lock AS (
        SELECT pg_advisory_xact_lock(hashtextextended(${revocationGateLockKey(target)}, 0))
      ),
      claimed AS (
        UPDATE ${workspaceProviderIntegration} AS target
        SET "revocation_pending_at" =
              COALESCE(target."revocation_pending_at", ${claimedAt}),
            "revocation_claimed_at" = ${claimedAt},
            "revocation_claim_id" = ${claimId}::uuid
        FROM gate_lock
        WHERE target."id" = ${target.integrationId}::uuid
          AND target."organization_id" = ${target.organizationId}
          AND target."status" = 'active'
          AND target."revoked_at" IS NULL
          AND (
            target."revocation_claim_id" IS NULL
            OR target."revocation_claimed_at" < ${staleBefore}
          )
        RETURNING target."revocation_pending_at" AS "pendingAt"
      )
      SELECT * FROM claimed
    `);
    rows = result.rows;
  }
  return parsedClaim(target, claimId, claimedAt, rows[0]);
}

async function updateClaim(
  claim: RevocationGateClaim,
  action: "release" | "clear" | "renew",
): Promise<RevocationGateClaim | boolean> {
  const nextClaimedAt = new Date();
  const nextClaimId = randomUUID();
  const values = action === "clear"
    ? sql`"revocation_pending_at" = NULL,
          "revocation_claimed_at" = NULL,
          "revocation_claim_id" = NULL`
    : action === "release"
      ? sql`"revocation_claimed_at" = NULL,
            "revocation_claim_id" = NULL`
      : sql`"revocation_claimed_at" = ${nextClaimedAt},
            "revocation_claim_id" = ${nextClaimId}::uuid`;
  let result;
  if (claim.kind === "member") {
    result = await db.execute<{ id: string }>(sql`
      UPDATE ${member}
      SET ${values}
      WHERE "id" = ${claim.memberId}
        AND "organization_id" = ${claim.organizationId}
        AND "user_id" = ${claim.userId}
        AND "revocation_pending_at" IS NOT NULL
        AND "revocation_claim_id" = ${claim.claimId}::uuid
      RETURNING "id"
    `);
  } else if (claim.kind === "connection") {
    result = await db.execute<{ id: string }>(sql`
      UPDATE ${workspaceConnection}
      SET ${values}
      WHERE "id" = ${claim.connectionId}::uuid
        AND "organization_id" = ${claim.organizationId}
        AND "deleted_at" IS NULL
        AND "revocation_pending_at" IS NOT NULL
        AND "revocation_claim_id" = ${claim.claimId}::uuid
      RETURNING "id"::text AS "id"
    `);
  } else {
    result = await db.execute<{ id: string }>(sql`
      UPDATE ${workspaceProviderIntegration}
      SET ${values}
      WHERE "id" = ${claim.integrationId}::uuid
        AND "organization_id" = ${claim.organizationId}
        AND "revocation_pending_at" IS NOT NULL
        AND "revocation_claim_id" = ${claim.claimId}::uuid
      RETURNING "id"::text AS "id"
    `);
  }
  if (result.rows.length !== 1) return false;
  if (action !== "renew") return true;
  return {
    ...claim,
    claimId: nextClaimId,
    claimedAt: nextClaimedAt,
  };
}

export async function releaseRevocationGateClaim(
  claim: RevocationGateClaim,
) {
  return updateClaim(claim, "release") as Promise<boolean>;
}

export async function clearRevocationGate(
  claim: RevocationGateClaim,
) {
  return updateClaim(claim, "clear") as Promise<boolean>;
}

export async function renewRevocationGateClaim(
  claim: RevocationGateClaim,
) {
  const renewed = await updateClaim(claim, "renew");
  return renewed === false ? null : renewed as RevocationGateClaim;
}

export type ManagedLeaseAuthority = {
  leaseId: string;
  organizationId: string;
  memberId: string;
  userId: string;
  role: WorkspaceRoleName;
  connectionId: string;
  connectionRevision: number;
  engine: "postgres" | "mysql";
  integrationId: string;
  provider: string;
  accessMode: ManagedAccessMode;
};

function memberGateKey(input: ManagedLeaseAuthority) {
  return `member:${input.organizationId}:${input.userId}`;
}

function connectionGateKey(input: ManagedLeaseAuthority) {
  return `connection:${input.organizationId}:${input.connectionId}`;
}

function integrationGateKey(input: ManagedLeaseAuthority) {
  return `integration:${input.organizationId}:${input.integrationId}`;
}

function capabilityPredicate(input: ManagedLeaseAuthority) {
  return input.accessMode === "write"
    ? sql`${member.role} IN ('editor', 'admin', 'owner')
        AND ${workspaceConnection.allowWrites} = TRUE`
    : sql`${member.role} IN ('analyst', 'editor', 'admin', 'owner')`;
}

function authorityPredicate(input: ManagedLeaseAuthority) {
  return sql`
    ${member.id} = ${input.memberId}
    AND ${member.organizationId} = ${input.organizationId}
    AND ${member.userId} = ${input.userId}
    AND ${member.role} = ${input.role}
    AND ${member.revocationPendingAt} IS NULL
    AND ${member.revocationClaimId} IS NULL
    AND ${capabilityPredicate(input)}
    AND ${workspaceConnection.id} = ${input.connectionId}::uuid
    AND ${workspaceConnection.organizationId} = ${input.organizationId}
    AND ${workspaceConnection.deletedAt} IS NULL
    AND ${workspaceConnection.revocationPendingAt} IS NULL
    AND ${workspaceConnection.revocationClaimId} IS NULL
    AND ${workspaceConnection.credentialMode} = 'managed'
    AND ${workspaceConnection.providerIntegrationId} = ${input.integrationId}::uuid
    AND ${workspaceConnection.revision} = ${input.connectionRevision}
    AND ${workspaceConnection.engine} = ${input.engine}
    AND ${workspaceConnection.provider} = ${input.provider}
    AND ${workspaceProviderIntegration.id} = ${input.integrationId}::uuid
    AND ${workspaceProviderIntegration.organizationId} = ${input.organizationId}
    AND ${workspaceProviderIntegration.provider} = ${input.provider}
    AND ${workspaceProviderIntegration.status} = 'active'
    AND ${workspaceProviderIntegration.revokedAt} IS NULL
    AND ${workspaceProviderIntegration.revocationPendingAt} IS NULL
    AND ${workspaceProviderIntegration.revocationClaimId} IS NULL
  `;
}

function authorityLockStatement(input: ManagedLeaseAuthority) {
  return sql`
    member_gate_lock AS MATERIALIZED (
      SELECT pg_advisory_xact_lock_shared(
        hashtextextended(${memberGateKey(input)}, 0)
      )
    ),
    connection_gate_lock AS MATERIALIZED (
      SELECT pg_advisory_xact_lock_shared(
        hashtextextended(${connectionGateKey(input)}, 0)
      )
      FROM member_gate_lock
    ),
    integration_gate_lock AS MATERIALIZED (
      SELECT pg_advisory_xact_lock_shared(
        hashtextextended(${integrationGateKey(input)}, 0)
      )
      FROM connection_gate_lock
    )
    SELECT 1 AS "locked" FROM integration_gate_lock
  `;
}

export async function reserveManagedLeaseIfUnblocked(
  input: ManagedLeaseAuthority,
) {
  const pendingExpiresAt = new Date(Date.now() + PENDING_LEASE_SECONDS * 1_000);
  const [, result] = await db.batch([
    db.execute(sql`WITH ${authorityLockStatement(input)}`),
    db.execute<{ status: string }>(sql`
    WITH authority AS (
      SELECT 1 AS "allowed"
      FROM ${member}, ${workspaceConnection}, ${workspaceProviderIntegration}
      WHERE ${authorityPredicate(input)}
    ),
    free_slots AS (
      SELECT slot."value" AS "value"
      FROM authority
      CROSS JOIN generate_series(1, 5) AS slot("value")
      WHERE NOT EXISTS (
        SELECT 1
        FROM ${workspaceCredentialLease} AS active_lease
        WHERE active_lease."organization_id" = ${input.organizationId}
          AND active_lease."connection_id" = ${input.connectionId}::uuid
          AND active_lease."user_id" = ${input.userId}
          AND active_lease."active_slot" = slot."value"
          AND active_lease."revoked_at" IS NULL
      )
      ORDER BY slot."value"
    ),
    inserted AS (
      INSERT INTO ${workspaceCredentialLease}
        ("id", "organization_id", "connection_id", "integration_id", "user_id",
         "provider", "access_mode", "external_credential_id",
         "external_credential_kind", "active_slot", "expires_at")
      SELECT ${input.leaseId}::uuid, ${input.organizationId},
             ${input.connectionId}::uuid, ${input.integrationId}::uuid,
             ${input.userId}, ${input.provider}, ${input.accessMode},
             ${input.leaseId}, 'pending', free_slots."value", ${pendingExpiresAt}
      FROM free_slots
      ORDER BY free_slots."value"
      ON CONFLICT DO NOTHING
      RETURNING "id"
    )
    SELECT CASE
      WHEN EXISTS (SELECT 1 FROM inserted) THEN 'reserved'
      WHEN NOT EXISTS (SELECT 1 FROM authority) THEN 'blocked'
      ELSE 'limit'
    END AS "status"
  `),
  ]);
  const status = result.rows[0]?.status;
  if (status !== "reserved" && status !== "blocked" && status !== "limit") {
    throw new Error("Invalid managed lease reservation result");
  }
  return status;
}

export async function finalizeManagedLeaseIfUnblocked(
  input: ManagedLeaseAuthority,
  lease: ManagedProviderLease,
) {
  const expiresAt = new Date(lease.expiresAt);
  if (Number.isNaN(expiresAt.valueOf())) return false;
  const [, result] = await db.batch([
    db.execute(sql`WITH ${authorityLockStatement(input)}`),
    db.execute<{ id: string }>(sql`
    UPDATE ${workspaceCredentialLease} AS lease
    SET "external_credential_id" = ${lease.externalCredentialId},
        "external_credential_kind" = ${lease.externalCredentialKind},
        "expires_at" = ${expiresAt}
    FROM ${member}, ${workspaceConnection}, ${workspaceProviderIntegration}
    WHERE ${authorityPredicate(input)}
      AND lease."id" = ${input.leaseId}::uuid
      AND lease."organization_id" = ${input.organizationId}
      AND lease."connection_id" = ${input.connectionId}::uuid
      AND lease."integration_id" = ${input.integrationId}::uuid
      AND lease."user_id" = ${input.userId}
      AND lease."provider" = ${input.provider}
      AND lease."access_mode" = ${input.accessMode}
      AND lease."external_credential_kind" = 'pending'
      AND lease."revoked_at" IS NULL
      AND lease."expires_at" > CURRENT_TIMESTAMP
      AND ${expiresAt} > CURRENT_TIMESTAMP
    RETURNING lease."id"::text AS "id"
  `),
  ]);
  return result.rows.length === 1;
}

export async function managedLeaseStillDeliverable(
  input: ManagedLeaseAuthority,
  lease: ManagedProviderLease,
) {
  const expiresAt = new Date(lease.expiresAt);
  if (Number.isNaN(expiresAt.valueOf())) return false;
  const [, result] = await db.batch([
    db.execute(sql`WITH ${authorityLockStatement(input)}`),
    db.execute<{ id: string }>(sql`
    SELECT lease."id"::text AS "id"
    FROM ${member}, ${workspaceConnection}, ${workspaceProviderIntegration},
         ${workspaceCredentialLease} AS lease
    WHERE ${authorityPredicate(input)}
      AND lease."id" = ${input.leaseId}::uuid
      AND lease."organization_id" = ${input.organizationId}
      AND lease."connection_id" = ${input.connectionId}::uuid
      AND lease."integration_id" = ${input.integrationId}::uuid
      AND lease."user_id" = ${input.userId}
      AND lease."provider" = ${input.provider}
      AND lease."access_mode" = ${input.accessMode}
      AND lease."external_credential_id" = ${lease.externalCredentialId}
      AND lease."external_credential_kind" = ${lease.externalCredentialKind}
      AND lease."external_credential_kind" <> 'pending'
      AND lease."expires_at" = ${expiresAt}
      AND lease."expires_at" > CURRENT_TIMESTAMP
      AND lease."revoked_at" IS NULL
    LIMIT 1
  `),
  ]);
  return result.rows.length === 1;
}
