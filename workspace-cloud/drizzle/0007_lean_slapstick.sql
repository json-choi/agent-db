CREATE TABLE "workspace_control"."workspace_provider_principal_claim" (
	"principal_fingerprint" text PRIMARY KEY NOT NULL,
	"organization_id" text NOT NULL,
	"integration_id" uuid NOT NULL,
	"target_fingerprint" text NOT NULL,
	"access_kind" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "provider_principal_claim_principal_hash" CHECK ("workspace_control"."workspace_provider_principal_claim"."principal_fingerprint" ~ '^[0-9a-f]{64}$'),
	CONSTRAINT "provider_principal_claim_target_hash" CHECK ("workspace_control"."workspace_provider_principal_claim"."target_fingerprint" ~ '^[0-9a-f]{64}$'),
	CONSTRAINT "provider_principal_claim_access_kind" CHECK ("workspace_control"."workspace_provider_principal_claim"."access_kind" IN ('read', 'write'))
);
--> statement-breakpoint
CREATE UNIQUE INDEX "provider_principal_claim_integration_access_idx" ON "workspace_control"."workspace_provider_principal_claim" USING btree ("integration_id","access_kind");--> statement-breakpoint
CREATE UNIQUE INDEX "provider_principal_claim_org_target_idx" ON "workspace_control"."workspace_provider_principal_claim" USING btree ("organization_id","target_fingerprint") WHERE "access_kind" = 'read';--> statement-breakpoint
CREATE INDEX "provider_principal_claim_target_idx" ON "workspace_control"."workspace_provider_principal_claim" USING btree ("target_fingerprint");--> statement-breakpoint
CREATE UNIQUE INDEX "workspace_connection_org_id_idx" ON "workspace_control"."workspace_connection" USING btree ("organization_id","id");--> statement-breakpoint
CREATE UNIQUE INDEX "provider_integration_org_id_idx" ON "workspace_control"."workspace_provider_integration" USING btree ("organization_id","id");--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_provider_principal_claim" ADD CONSTRAINT "workspace_provider_principal_claim_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "workspace_control"."organization"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_provider_principal_claim" ADD CONSTRAINT "provider_principal_claim_org_integration_fk" FOREIGN KEY ("organization_id","integration_id") REFERENCES "workspace_control"."workspace_provider_integration"("organization_id","id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
DO $migration$
BEGIN
	IF EXISTS (
		SELECT 1
		FROM "workspace_control"."workspace_connection" AS connection
		INNER JOIN "workspace_control"."workspace_provider_integration" AS integration
			ON integration."id" = connection."provider_integration_id"
		WHERE connection."organization_id" <> integration."organization_id"
	) OR EXISTS (
		SELECT 1
		FROM "workspace_control"."workspace_credential_lease" AS lease
		INNER JOIN "workspace_control"."workspace_connection" AS connection
			ON connection."id" = lease."connection_id"
		WHERE lease."organization_id" <> connection."organization_id"
	) OR EXISTS (
		SELECT 1
		FROM "workspace_control"."workspace_credential_lease" AS lease
		INNER JOIN "workspace_control"."workspace_provider_integration" AS integration
			ON integration."id" = lease."integration_id"
		WHERE lease."organization_id" <> integration."organization_id"
	) THEN
		RAISE EXCEPTION 'workspace tenant relationship mismatch; refusing security migration';
	END IF;
END
$migration$;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_connection" ADD CONSTRAINT "workspace_connection_org_provider_integration_fk" FOREIGN KEY ("organization_id","provider_integration_id") REFERENCES "workspace_control"."workspace_provider_integration"("organization_id","id") ON DELETE no action ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_credential_lease" ADD CONSTRAINT "credential_lease_org_connection_fk" FOREIGN KEY ("organization_id","connection_id") REFERENCES "workspace_control"."workspace_connection"("organization_id","id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_credential_lease" ADD CONSTRAINT "credential_lease_org_integration_fk" FOREIGN KEY ("organization_id","integration_id") REFERENCES "workspace_control"."workspace_provider_integration"("organization_id","id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
DO $migration$
BEGIN
	IF EXISTS (
		SELECT 1
		FROM "workspace_control"."workspace_provider_integration"
		WHERE "provider" = 'gcpCloudSql'
			AND "status" = 'active'
			AND "revoked_at" IS NULL
			AND "external_account_id" !~
				'^gcp-wif-v1:r[0-9a-f]{64}:w(none|[0-9a-f]{64}):n[0-9a-f]{64}:i[0-9a-f]{64}$'
	) THEN
		RAISE EXCEPTION 'invalid active GCP integration identity; refusing principal backfill';
	END IF;

	IF EXISTS (
		WITH parsed AS (
			SELECT integration."id" AS integration_id,
						 integration."organization_id" AS organization_id,
						 fingerprints[1] AS read_principal,
						 fingerprints[2] AS write_principal,
						 fingerprints[3] AS target_fingerprint
			FROM "workspace_control"."workspace_provider_integration" AS integration
			CROSS JOIN LATERAL regexp_match(
				integration."external_account_id",
				'^gcp-wif-v1:r([0-9a-f]{64}):w(none|[0-9a-f]{64}):n([0-9a-f]{64}):i([0-9a-f]{64})$'
			) AS parsed_identity(fingerprints)
			WHERE integration."provider" = 'gcpCloudSql'
				AND integration."status" = 'active'
				AND integration."revoked_at" IS NULL
		),
		claims AS (
			SELECT read_principal AS principal_fingerprint,
						 organization_id,
						 target_fingerprint
			FROM parsed
			UNION ALL
			SELECT write_principal, organization_id, target_fingerprint
			FROM parsed
			WHERE write_principal <> 'none'
		)
		SELECT principal_fingerprint
		FROM claims
		GROUP BY principal_fingerprint
		HAVING COUNT(*) > 1
	) THEN
		RAISE EXCEPTION 'duplicate active GCP service-account claim; refusing principal backfill';
	END IF;

	IF EXISTS (
		WITH parsed AS (
			SELECT integration."organization_id" AS organization_id,
						 fingerprints[3] AS target_fingerprint
			FROM "workspace_control"."workspace_provider_integration" AS integration
			CROSS JOIN LATERAL regexp_match(
				integration."external_account_id",
				'^gcp-wif-v1:r([0-9a-f]{64}):w(none|[0-9a-f]{64}):n([0-9a-f]{64}):i([0-9a-f]{64})$'
			) AS parsed_identity(fingerprints)
			WHERE integration."provider" = 'gcpCloudSql'
				AND integration."status" = 'active'
				AND integration."revoked_at" IS NULL
		)
		SELECT target_fingerprint
		FROM parsed
		GROUP BY organization_id, target_fingerprint
		HAVING COUNT(*) > 1
	) THEN
		RAISE EXCEPTION 'duplicate workspace GCP target; refusing principal backfill';
	END IF;
END
$migration$;--> statement-breakpoint
WITH parsed AS (
	SELECT integration."id" AS integration_id,
				 integration."organization_id" AS organization_id,
				 integration."created_at" AS created_at,
				 integration."updated_at" AS updated_at,
				 fingerprints[1] AS read_principal,
				 fingerprints[2] AS write_principal,
				 fingerprints[3] AS target_fingerprint
	FROM "workspace_control"."workspace_provider_integration" AS integration
	CROSS JOIN LATERAL regexp_match(
		integration."external_account_id",
		'^gcp-wif-v1:r([0-9a-f]{64}):w(none|[0-9a-f]{64}):n([0-9a-f]{64}):i([0-9a-f]{64})$'
	) AS parsed_identity(fingerprints)
	WHERE integration."provider" = 'gcpCloudSql'
		AND integration."status" = 'active'
		AND integration."revoked_at" IS NULL
),
claims AS (
	SELECT read_principal AS principal_fingerprint,
				 organization_id,
				 integration_id,
				 target_fingerprint,
				 'read'::text AS access_kind,
				 created_at,
				 updated_at
	FROM parsed
	UNION ALL
	SELECT write_principal,
				 organization_id,
				 integration_id,
				 target_fingerprint,
				 'write'::text,
				 created_at,
				 updated_at
	FROM parsed
	WHERE write_principal <> 'none'
)
INSERT INTO "workspace_control"."workspace_provider_principal_claim"
	("principal_fingerprint", "organization_id", "integration_id",
	 "target_fingerprint", "access_kind", "created_at", "updated_at")
SELECT principal_fingerprint, organization_id, integration_id, target_fingerprint,
			 access_kind, created_at, updated_at
FROM claims;
