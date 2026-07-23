CREATE TABLE "workspace_control"."provider_oauth_state" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"organization_id" text NOT NULL,
	"user_id" text NOT NULL,
	"provider" text NOT NULL,
	"state_hash" text NOT NULL,
	"expires_at" timestamp with time zone NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "workspace_control"."workspace_credential_lease" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"organization_id" text NOT NULL,
	"connection_id" uuid NOT NULL,
	"integration_id" uuid NOT NULL,
	"user_id" text NOT NULL,
	"provider" text NOT NULL,
	"access_mode" text NOT NULL,
	"external_credential_id" text NOT NULL,
	"external_credential_kind" text NOT NULL,
	"expires_at" timestamp with time zone NOT NULL,
	"revoked_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "workspace_control"."workspace_provider_integration" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"organization_id" text NOT NULL,
	"provider" text NOT NULL,
	"status" text DEFAULT 'active' NOT NULL,
	"external_account_id" text NOT NULL,
	"display_name" text NOT NULL,
	"encrypted_credential" text NOT NULL,
	"credential_expires_at" timestamp with time zone,
	"granted_scope" text,
	"created_by_user_id" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	"revoked_at" timestamp with time zone
);
--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_connection" ADD COLUMN "credential_mode" text DEFAULT 'member_local' NOT NULL;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_connection" ADD COLUMN "provider_integration_id" uuid;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_connection" ADD COLUMN "provider_resource" jsonb;--> statement-breakpoint
ALTER TABLE "workspace_control"."provider_oauth_state" ADD CONSTRAINT "provider_oauth_state_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "workspace_control"."organization"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."provider_oauth_state" ADD CONSTRAINT "provider_oauth_state_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "workspace_control"."user"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_credential_lease" ADD CONSTRAINT "workspace_credential_lease_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "workspace_control"."organization"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_credential_lease" ADD CONSTRAINT "workspace_credential_lease_connection_id_workspace_connection_id_fk" FOREIGN KEY ("connection_id") REFERENCES "workspace_control"."workspace_connection"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_credential_lease" ADD CONSTRAINT "workspace_credential_lease_integration_id_workspace_provider_integration_id_fk" FOREIGN KEY ("integration_id") REFERENCES "workspace_control"."workspace_provider_integration"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_credential_lease" ADD CONSTRAINT "workspace_credential_lease_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "workspace_control"."user"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_provider_integration" ADD CONSTRAINT "workspace_provider_integration_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "workspace_control"."organization"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_provider_integration" ADD CONSTRAINT "workspace_provider_integration_created_by_user_id_user_id_fk" FOREIGN KEY ("created_by_user_id") REFERENCES "workspace_control"."user"("id") ON DELETE set null ON UPDATE no action;--> statement-breakpoint
CREATE UNIQUE INDEX "provider_oauth_state_hash_idx" ON "workspace_control"."provider_oauth_state" USING btree ("state_hash");--> statement-breakpoint
CREATE INDEX "provider_oauth_state_expiry_idx" ON "workspace_control"."provider_oauth_state" USING btree ("expires_at");--> statement-breakpoint
CREATE INDEX "credential_lease_member_active_idx" ON "workspace_control"."workspace_credential_lease" USING btree ("organization_id","user_id","expires_at");--> statement-breakpoint
CREATE INDEX "credential_lease_connection_active_idx" ON "workspace_control"."workspace_credential_lease" USING btree ("connection_id","expires_at");--> statement-breakpoint
CREATE UNIQUE INDEX "provider_integration_org_provider_account_idx" ON "workspace_control"."workspace_provider_integration" USING btree ("organization_id","provider","external_account_id");--> statement-breakpoint
CREATE INDEX "provider_integration_org_status_idx" ON "workspace_control"."workspace_provider_integration" USING btree ("organization_id","status");--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_connection" ADD CONSTRAINT "workspace_connection_provider_integration_id_workspace_provider_integration_id_fk" FOREIGN KEY ("provider_integration_id") REFERENCES "workspace_control"."workspace_provider_integration"("id") ON DELETE set null ON UPDATE no action;