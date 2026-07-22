-- One-time pre-production cutover from the hand-written prototype schema.
-- The migration runner refuses this reset when legacy identity/workspace rows exist.
DO $cutover$
DECLARE
  legacy_rows bigint := 0;
BEGIN
  IF to_regclass('workspace_control.users') IS NOT NULL THEN
    EXECUTE 'SELECT count(*) FROM workspace_control.users' INTO legacy_rows;
  END IF;
  IF to_regclass('workspace_control.workspaces') IS NOT NULL THEN
    EXECUTE 'SELECT count(*) + $1 FROM workspace_control.workspaces'
      INTO legacy_rows USING legacy_rows;
  END IF;
  IF legacy_rows > 0 THEN
    RAISE EXCEPTION 'Refusing Better Auth/Drizzle cutover: legacy users or workspaces contain data';
  END IF;
END
$cutover$;
--> statement-breakpoint
DROP SCHEMA IF EXISTS "workspace_control" CASCADE;
--> statement-breakpoint
CREATE EXTENSION IF NOT EXISTS "pgcrypto";
--> statement-breakpoint
CREATE SCHEMA "workspace_control";
--> statement-breakpoint
CREATE TABLE "workspace_control"."account" (
	"id" text PRIMARY KEY NOT NULL,
	"account_id" text NOT NULL,
	"provider_id" text NOT NULL,
	"user_id" text NOT NULL,
	"access_token" text,
	"refresh_token" text,
	"id_token" text,
	"access_token_expires_at" timestamp with time zone,
	"refresh_token_expires_at" timestamp with time zone,
	"scope" text,
	"password" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "workspace_control"."device_code" (
	"id" text PRIMARY KEY NOT NULL,
	"device_code" text NOT NULL,
	"user_code" text NOT NULL,
	"user_id" text,
	"expires_at" timestamp with time zone NOT NULL,
	"status" text NOT NULL,
	"last_polled_at" timestamp with time zone,
	"polling_interval" integer,
	"client_id" text,
	"scope" text,
	CONSTRAINT "device_code_device_code_unique" UNIQUE("device_code"),
	CONSTRAINT "device_code_user_code_unique" UNIQUE("user_code")
);
--> statement-breakpoint
CREATE TABLE "workspace_control"."invitation" (
	"id" text PRIMARY KEY NOT NULL,
	"organization_id" text NOT NULL,
	"email" text NOT NULL,
	"role" text,
	"status" text DEFAULT 'pending' NOT NULL,
	"expires_at" timestamp with time zone NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"inviter_id" text NOT NULL
);
--> statement-breakpoint
CREATE TABLE "workspace_control"."member" (
	"id" text PRIMARY KEY NOT NULL,
	"organization_id" text NOT NULL,
	"user_id" text NOT NULL,
	"role" text DEFAULT 'viewer' NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "workspace_control"."organization" (
	"id" text PRIMARY KEY NOT NULL,
	"name" text NOT NULL,
	"slug" text NOT NULL,
	"logo" text,
	"metadata" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "organization_slug_unique" UNIQUE("slug")
);
--> statement-breakpoint
CREATE TABLE "workspace_control"."rate_limit" (
	"id" text PRIMARY KEY NOT NULL,
	"key" text NOT NULL,
	"count" integer NOT NULL,
	"last_request" bigint NOT NULL,
	CONSTRAINT "rate_limit_key_unique" UNIQUE("key")
);
--> statement-breakpoint
CREATE TABLE "workspace_control"."session" (
	"id" text PRIMARY KEY NOT NULL,
	"expires_at" timestamp with time zone NOT NULL,
	"token" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	"ip_address" text,
	"user_agent" text,
	"user_id" text NOT NULL,
	"active_organization_id" text,
	CONSTRAINT "session_token_unique" UNIQUE("token")
);
--> statement-breakpoint
CREATE TABLE "workspace_control"."user" (
	"id" text PRIMARY KEY NOT NULL,
	"name" text NOT NULL,
	"email" text NOT NULL,
	"email_verified" boolean DEFAULT false NOT NULL,
	"image" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "user_email_unique" UNIQUE("email")
);
--> statement-breakpoint
CREATE TABLE "workspace_control"."verification" (
	"id" text PRIMARY KEY NOT NULL,
	"identifier" text NOT NULL,
	"value" text NOT NULL,
	"expires_at" timestamp with time zone NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "workspace_control"."workspace_audit_event" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
	"organization_id" text NOT NULL,
	"actor_user_id" text,
	"action" text NOT NULL,
	"resource_type" text NOT NULL,
	"resource_id" text,
	"redacted_summary" jsonb DEFAULT '{}'::jsonb NOT NULL,
	"request_id" uuid NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "workspace_control"."workspace_profile" (
	"organization_id" text PRIMARY KEY NOT NULL,
	"lifecycle_state" text DEFAULT 'active' NOT NULL,
	"encryption_key_ref" text NOT NULL,
	"residency_region" text,
	"revision" bigint DEFAULT 1 NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
ALTER TABLE "workspace_control"."account" ADD CONSTRAINT "account_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "workspace_control"."user"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."device_code" ADD CONSTRAINT "device_code_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "workspace_control"."user"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."invitation" ADD CONSTRAINT "invitation_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "workspace_control"."organization"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."invitation" ADD CONSTRAINT "invitation_inviter_id_user_id_fk" FOREIGN KEY ("inviter_id") REFERENCES "workspace_control"."user"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."member" ADD CONSTRAINT "member_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "workspace_control"."organization"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."member" ADD CONSTRAINT "member_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "workspace_control"."user"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."session" ADD CONSTRAINT "session_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "workspace_control"."user"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."session" ADD CONSTRAINT "session_active_organization_id_organization_id_fk" FOREIGN KEY ("active_organization_id") REFERENCES "workspace_control"."organization"("id") ON DELETE set null ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_audit_event" ADD CONSTRAINT "workspace_audit_event_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "workspace_control"."organization"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_audit_event" ADD CONSTRAINT "workspace_audit_event_actor_user_id_user_id_fk" FOREIGN KEY ("actor_user_id") REFERENCES "workspace_control"."user"("id") ON DELETE set null ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "workspace_control"."workspace_profile" ADD CONSTRAINT "workspace_profile_organization_id_organization_id_fk" FOREIGN KEY ("organization_id") REFERENCES "workspace_control"."organization"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
CREATE INDEX "account_user_idx" ON "workspace_control"."account" USING btree ("user_id");--> statement-breakpoint
CREATE UNIQUE INDEX "account_provider_subject_idx" ON "workspace_control"."account" USING btree ("provider_id","account_id");--> statement-breakpoint
CREATE INDEX "device_code_user_idx" ON "workspace_control"."device_code" USING btree ("user_id");--> statement-breakpoint
CREATE INDEX "invitation_organization_idx" ON "workspace_control"."invitation" USING btree ("organization_id");--> statement-breakpoint
CREATE INDEX "invitation_email_idx" ON "workspace_control"."invitation" USING btree ("email");--> statement-breakpoint
CREATE UNIQUE INDEX "member_organization_user_idx" ON "workspace_control"."member" USING btree ("organization_id","user_id");--> statement-breakpoint
CREATE INDEX "member_user_idx" ON "workspace_control"."member" USING btree ("user_id");--> statement-breakpoint
CREATE INDEX "session_user_idx" ON "workspace_control"."session" USING btree ("user_id");--> statement-breakpoint
CREATE INDEX "verification_identifier_idx" ON "workspace_control"."verification" USING btree ("identifier");--> statement-breakpoint
CREATE INDEX "workspace_audit_org_created_idx" ON "workspace_control"."workspace_audit_event" USING btree ("organization_id","created_at");
