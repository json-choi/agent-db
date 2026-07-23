ALTER TABLE "workspace_control"."member" ADD COLUMN "revocation_pending_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "workspace_control"."member" ADD COLUMN "revocation_claimed_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "workspace_control"."member" ADD COLUMN "revocation_claim_id" uuid;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_connection" ADD COLUMN "revocation_pending_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_connection" ADD COLUMN "revocation_claimed_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_connection" ADD COLUMN "revocation_claim_id" uuid;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_credential_lease" ADD COLUMN "active_slot" integer;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_provider_integration" ADD COLUMN "revocation_pending_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_provider_integration" ADD COLUMN "revocation_claimed_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_provider_integration" ADD COLUMN "revocation_claim_id" uuid;--> statement-breakpoint
WITH "ranked_active_lease" AS (
  SELECT "id",
         ROW_NUMBER() OVER (
           PARTITION BY "organization_id", "connection_id", "user_id"
           ORDER BY "expires_at" DESC, "id"
         ) AS "slot"
  FROM "workspace_control"."workspace_credential_lease"
  WHERE "revoked_at" IS NULL
)
UPDATE "workspace_control"."workspace_credential_lease" AS "lease"
SET "active_slot" = "ranked_active_lease"."slot"
FROM "ranked_active_lease"
WHERE "lease"."id" = "ranked_active_lease"."id"
  AND "ranked_active_lease"."slot" <= 5;--> statement-breakpoint
CREATE UNIQUE INDEX "credential_lease_active_slot_idx" ON "workspace_control"."workspace_credential_lease" USING btree ("organization_id","connection_id","user_id","active_slot") WHERE "revoked_at" IS NULL;--> statement-breakpoint
ALTER TABLE "workspace_control"."member" ADD CONSTRAINT "member_revocation_claim_consistent" CHECK (("workspace_control"."member"."revocation_claimed_at" IS NULL AND "workspace_control"."member"."revocation_claim_id" IS NULL)
        OR ("workspace_control"."member"."revocation_claimed_at" IS NOT NULL
          AND "workspace_control"."member"."revocation_claim_id" IS NOT NULL
          AND "workspace_control"."member"."revocation_pending_at" IS NOT NULL));--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_connection" ADD CONSTRAINT "workspace_connection_revocation_claim_consistent" CHECK (("workspace_control"."workspace_connection"."revocation_claimed_at" IS NULL AND "workspace_control"."workspace_connection"."revocation_claim_id" IS NULL)
        OR ("workspace_control"."workspace_connection"."revocation_claimed_at" IS NOT NULL
          AND "workspace_control"."workspace_connection"."revocation_claim_id" IS NOT NULL
          AND "workspace_control"."workspace_connection"."revocation_pending_at" IS NOT NULL));--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_credential_lease" ADD CONSTRAINT "credential_lease_active_slot_range" CHECK ("workspace_control"."workspace_credential_lease"."active_slot" IS NULL OR "workspace_control"."workspace_credential_lease"."active_slot" BETWEEN 1 AND 5);--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_credential_lease" ADD CONSTRAINT "credential_lease_live_slot_required" CHECK ("workspace_control"."workspace_credential_lease"."revoked_at" IS NOT NULL OR "workspace_control"."workspace_credential_lease"."active_slot" IS NOT NULL);--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_provider_integration" ADD CONSTRAINT "provider_integration_revocation_claim_consistent" CHECK (("workspace_control"."workspace_provider_integration"."revocation_claimed_at" IS NULL AND "workspace_control"."workspace_provider_integration"."revocation_claim_id" IS NULL)
        OR ("workspace_control"."workspace_provider_integration"."revocation_claimed_at" IS NOT NULL
          AND "workspace_control"."workspace_provider_integration"."revocation_claim_id" IS NOT NULL
          AND "workspace_control"."workspace_provider_integration"."revocation_pending_at" IS NOT NULL));
