# DopeDB Workspace Cloud

This is the authenticated web and API control plane for DopeDB workspaces. It is a
separate Next.js application intended for its own Vercel project at `app.dopedb.dev`;
the marketing `site/` deployment remains independent.

## Local setup

Copy `.env.example` to the ignored `workspace-cloud/.env.local` and provide the Neon
pooler/unpooled URLs, Google OAuth web client credentials, a Better Auth secret, the
exact Better Auth URL, and a random 32-byte base64url `WORKSPACE_CREDENTIAL_KEY`.
PlanetScale managed access additionally requires `PLANETSCALE_CLIENT_ID` and
`PLANETSCALE_CLIENT_SECRET`; Neon and GCP Cloud SQL do not add application
environment secrets. Set a separate random `CRON_SECRET` for the authenticated
credential-cleanup route. The committed one-minute schedule requires Vercel Pro or
Enterprise; do not deploy Neon managed access with a daily-only Hobby cron. Register
this PlanetScale callback:

```text
http://localhost:3000/api/v1/providers/planet-scale/callback
```

Configure the PlanetScale OAuth application with the minimum scopes used by the
managed-access flow: `read_organizations`, `read_databases`, `read_branches`,
`manage_passwords`, and `manage_production_branch_passwords`. The callback rejects a
grant missing any of them instead of leaving a partially working integration.

To deliver invitation email, also set `RESEND_API_KEY` and a
verified `WORKSPACE_INVITATION_FROM` sender; without them, the dashboard keeps the
email-bound copy-link fallback. Configure this Google redirect URI:

```text
http://localhost:3000/api/auth/callback/google
```

Then run `pnpm install` in this directory and `pnpm workspace:cloud:dev` from the repo
root. Generate/check migrations with `pnpm db:generate` and `pnpm db:check` here; apply
them through the unpooled URL with `pnpm workspace:migrate` from the repository root.

## Neon managed access

1. Create a **project-scoped organization API key** in Neon when the project belongs
   to an organization. A personal key also works, but has a wider account blast radius.
2. In Workspace settings, choose Neon, enter the key and optional organization ID,
   and select project → branch → database.
3. DopeDB retrieves an owner connection only on the server, creates a unique login
   role with a 15-minute password validity, and grants that role only `CONNECT` plus
   current table/sequence privileges in an explicit schema allowlist (`public` by
   default). It does not use the Neon API role endpoint because API-created roles
   inherit `neon_superuser`.

Managed mode fails closed unless the selected database is the branch's only
PUBLIC-connectable non-template database. Use an isolated branch, or revoke
`PUBLIC CONNECT` on every other database first. PostgreSQL grants `TEMPORARY` on
new databases to `PUBLIC` by default; remove both database-level object-creation
paths on the selected database before enabling managed access:

```sql
REVOKE CREATE, TEMPORARY ON DATABASE "DATABASE_NAME" FROM PUBLIC;
```

DopeDB verifies the effective ACL including PostgreSQL's implicit default when
`datacl` is null. It rejects `PUBLIC CREATE` because that can create a schema
outside the allowlist, and rejects `PUBLIC TEMPORARY` because even a read lease
could otherwise perform unscoped temporary writes. Reserved provider schemas
(`neon`, `neon_auth`, `pg_*`, and `information_schema`) cannot be selected. Every
selected object must be grantable by the database owner, and selected schemas must not
expose a `SECURITY DEFINER` function through `PUBLIC EXECUTE`. Managed mode also
rejects PUBLIC schema/object privileges that would let the lease role escape the
allowlist or exceed its read/write mode.

The API key is envelope-encrypted at rest and never returned. Disconnecting DopeDB
scrubs its encrypted copy; it intentionally does not delete a customer-owned Neon key
that another integration might use. The integration identity is derived from the
current Neon user/organization and a fingerprint of exactly the accessible project
IDs, so rotating a key with the same scope updates one integration while a narrower
project key remains separate. Revoke an unused key in Neon.

Role passwords are sent to PostgreSQL only as client-generated SCRAM-SHA-256
verifiers. The desktop uses the direct Neon endpoint, limits the two leased pools to
four combined connections, and closes them 30 seconds before expiry. The authenticated
Vercel cron independently commits `NOLOGIN`, terminates remaining sessions, and removes
expired roles. Vercel cron scheduling is not an exact timer, so the documentation does
not treat password `VALID UNTIL` alone as a hard session-expiry boundary.

## GCP Cloud SQL managed access

GCP uses keyless federation. Do not create or upload a JSON service-account key.

1. Enable `sts.googleapis.com`, `iamcredentials.googleapis.com`, and
   `sqladmin.googleapis.com`. Enable Vercel OIDC for this exact `workspace-cloud`
   Vercel project and create a GCP Workload Identity Pool/provider. Map at least
   `google.subject=assertion.sub`, `attribute.project_id=assertion.project_id`, and
   `attribute.environment=assertion.environment`. Its attribute condition must require
   both the exact Vercel `project_id` and `environment == 'production'`; do not trust
   the whole pool or every deployment owned by the Vercel team.

   ```sh
   gcloud services enable sts.googleapis.com iamcredentials.googleapis.com \
     sqladmin.googleapis.com --project=PROJECT_ID
   gcloud iam workload-identity-pools providers create-oidc PROVIDER_ID \
     --project=PROJECT_ID --location=global --workload-identity-pool=POOL_ID \
     --issuer-uri=https://oidc.vercel.com/TEAM_SLUG \
     --allowed-audiences=https://vercel.com/TEAM_SLUG \
     --attribute-mapping="google.subject=assertion.sub,attribute.project_id=assertion.project_id,attribute.environment=assertion.environment" \
     --attribute-condition="assertion.project_id == 'VERCEL_PROJECT_ID' && assertion.environment == 'production'"
   ```

   Use the global Vercel issuer instead when that is the issuer mode configured for
   the project; issuer URI and audience must match the actual Vercel token exactly.
2. For **each Cloud SQL instance**, create a dedicated read service account and,
   only when needed, a separate write service account. Grant the WIF principal
   `roles/iam.workloadIdentityUser` on only those accounts. The read account also needs
   Cloud SQL Viewer, scoped to the same instance condition in the next step. No Cloud
   SQL Admin or long-lived key is required.

   ```sh
   gcloud iam service-accounts add-iam-policy-binding SERVICE_ACCOUNT_EMAIL \
     --project=PROJECT_ID --role=roles/iam.workloadIdentityUser \
     --member="principalSet://iam.googleapis.com/projects/PROJECT_NUMBER/locations/global/workloadIdentityPools/POOL_ID/attribute.project_id/VERCEL_PROJECT_ID"
   ```
3. Grant each database identity `roles/cloudsql.instanceUser`, and the read identity
   `roles/cloudsql.viewer`, with this instance condition, replacing both placeholders:

   ```text
   resource.name == 'projects/PROJECT_ID/instances/INSTANCE_ID'
     && resource.service == 'sqladmin.googleapis.com'
   ```

   A project-wide Instance User grant is not acceptable for managed access. The
   15-minute OAuth token is scoped to the service-account identity, not to the selected
   database; this IAM Condition and the database grants are the authorization boundary.
   Do not reuse either service account for another Cloud SQL instance.

   ```sh
   gcloud projects add-iam-policy-binding PROJECT_ID \
     --member=serviceAccount:SERVICE_ACCOUNT_EMAIL \
     --role=roles/cloudsql.instanceUser \
     --condition="title=dopedb-INSTANCE_ID,expression=resource.name == 'projects/PROJECT_ID/instances/INSTANCE_ID' && resource.service == 'sqladmin.googleapis.com'"
   gcloud projects add-iam-policy-binding PROJECT_ID \
     --member=serviceAccount:READ_SERVICE_ACCOUNT_EMAIL \
     --role=roles/cloudsql.viewer \
     --condition="title=dopedb-view-INSTANCE_ID,expression=resource.name == 'projects/PROJECT_ID/instances/INSTANCE_ID' && resource.service == 'sqladmin.googleapis.com'"
   ```
4. Enable IAM database authentication on the instance
   (`cloudsql.iam_authentication` for PostgreSQL,
   `cloudsql_iam_authentication` for MySQL), add both identities as
   `CLOUD_IAM_SERVICE_ACCOUNT` database users, and grant the read identity only
   database read privileges and the write identity only the intended DML privileges.
   Remove default or inherited grants that would widen either role.
5. In Workspace settings, enter the project, WIF provider, dedicated instance, and
   service-account identities. Confirm the two security requirements and choose the
   exact desktop network path: Public IP, private services access, or Private Service
   Connect. DopeDB refuses to attach another instance to that integration and rejects a
   service account already registered for a different instance.

   The two confirmations are administrator attestations, not a complete IAM or
   database-privilege audit. DopeDB verifies that federation and impersonation work,
   the exact instance is runnable, IAM database users exist, and IAM database
   authentication is enabled. The minimal control-plane permissions intentionally do
   not allow DopeDB to prove every IAM Condition expression or inspect every database
   grant, so review those policies with the commands above before confirming them.

At lease time Vercel OIDC is exchanged through GCP STS and IAM Credentials for a
15-minute `sqlservice.login` token. Only that one-time token and the Cloud SQL server
CA reach the desktop process; the native driver pins the CA, and MySQL cleartext auth
is enabled only inside that verified TLS connection. The token is not revocable, so
access changes wait for its bounded expiry and the desktop drops its pool 30 seconds
early. Pool eviction prevents new app work but is not a protocol-level kill switch for
an already checked-out connection; the database's own statement/session limits remain
the final bound for a query already running.

DopeDB currently uses Cloud SQL's documented manual IAM connection path rather than
proxying database traffic. Public IP must authorize the member machine's network;
private services access and Private Service Connect must be resolvable and reachable
from that machine. For the per-instance internal CA, Public and private-services-access
IP connections use the unique instance CA; PSC uses hostname verification. Shared and
customer-managed CA modes never fall back to IP: they require an instance-scoped DNS
name and full hostname verification. Supported Cloud SQL names include `.sql.goog`,
`.sql-psa.goog`, and `.sql-psc.goog`; configure the corresponding DNS zone before
selecting a private path. Client-certificate-required instances are deliberately
rejected because this direct IAM flow does not issue client certificates.

Changing the WIF provider or dedicated service accounts for the same project and
instance rotates that integration in place. The server first gates new leases, drains
existing credentials, then atomically replaces hash-only global principal claims so a
service account cannot be reused by another integration. Selecting a different
dedicated instance creates a separate integration; move its connections before
disconnecting the old one.
GCP managed connections saved before the explicit network-path field was introduced
are intentionally not leased. A workspace admin must reapply managed access and select
Public IP, private services access, or Private Service Connect; the server does not
guess a path for legacy records.

## Trust boundary

- Better Auth owns Google login, sessions, organizations, invitations, rate limits, and
  RFC 8628 device authorization; the app does not maintain a parallel auth system.
- Database hooks clear Google access, refresh, and ID tokens before account persistence.
- Better Auth Multi Session keeps at most ten browser identities available without
  merging their users or organization memberships. The active identity is explicit.
- Desktop sign-in uses a ten-minute, single-use device code and a Better Auth Bearer
  session. Sessions expire after 30 days with a one-day refresh age, and the desktop
  stores each account in a separate operating-system credential item.
- All application queries use Drizzle ORM; all schema changes use committed Drizzle Kit
  migrations.
- Member-local target-database credentials never enter this service. In optional
  managed mode, reusable PlanetScale OAuth or Neon API authorization is AES-256-GCM
  encrypted with record-bound AAD before database persistence; GCP stores only
  non-secret WIF coordinates and service-account identities. The envelope key is held
  separately in deployment configuration.
- Managed target-database credentials are generated per member with a 15-minute TTL,
  returned once to an authenticated native Bearer client, and never inserted into the
  service database, audit stream, browser UI, or desktop store.
- Shared connection rows contain endpoint metadata, safety defaults, credential mode,
  and a redacted provider-resource selector. Usernames,
  passwords, tokens, certificates, connection URLs, SQLite paths, advanced parameters,
  and desktop `secret_ref` values are rejected or absent from the hosted schema.
- Endpoint metadata currently relies on HTTPS in transit and the managed database's
  storage controls. The roadmap's application-level per-workspace envelope encryption is
  not yet implemented, so this release must not be described as end-to-end encrypted.
- Admin/Owner can create, resend, and cancel Better Auth invitations; remove members;
  and assign Viewer (metadata only), Analyst (read-only), Editor (read/write through
  local safety gates), or Admin roles. Resend delivers email when configured, while the
  settings page always exposes a copyable, email-bound invitation link.
- A signed-in user with a verified Google email automatically accepts every live
  invitation for that exact email on the next workspace read. Better Auth still
  performs the recipient, expiry, role, membership-limit, and state-transition checks.
- Shared database execution uses a fresh server authorization check. Cached desktop role
  data is for presentation and fail-closed prechecks, not the final permission decision.
- Role downgrade, member removal, provider disconnect, and managed-mode changes attempt
  immediate provider credential revocation where supported. Neon additionally uses
  lazy and scheduled role cleanup because PostgreSQL `VALID UNTIL` does not terminate
  existing sessions. GCP IAM login tokens cannot be revoked, so GCP access changes wait
  for token expiry while the desktop closes its leased pools early.
- Identity, membership, invitation, and connection API responses are private `no-store`
  payloads and are covered by restrictive browser security headers.

## Security references

- [Better Auth Organization](https://better-auth.com/docs/plugins/organization) for
  invitations, verified-email acceptance, custom roles, and server-side membership.
- [OWASP Authorization Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Authorization_Cheat_Sheet.html)
  for least privilege, deny-by-default, per-request checks, and authorization tests.
- [OWASP Secrets Management Cheat Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Secrets_Management_Cheat_Sheet.html)
  for credential minimization, fine-grained access, non-logging, rotation, and revocation.
- [PostgreSQL role membership](https://www.postgresql.org/docs/current/role-membership.html)
  for the independent target-database privilege boundary.
- [Neon API authentication](https://api-docs.neon.tech/reference/authentication) for
  key types and project-scoped organization keys.
- [Neon current-user organizations](https://api-docs.neon.tech/reference/getcurrentuserorganizations)
  for identity resolution that also supports organization and project-scoped keys.
- [PostgreSQL CREATE ROLE](https://www.postgresql.org/docs/current/sql-createrole.html)
  for SCRAM verifiers and the password-only semantics of `VALID UNTIL`.
- [Vercel Cron security](https://vercel.com/docs/cron-jobs/manage-cron-jobs) for
  `CRON_SECRET` Bearer authentication and scheduling limitations.
- [Vercel OIDC for GCP](https://vercel.com/docs/oidc/gcp) and
  [Vercel OIDC claims](https://vercel.com/docs/oidc/reference) for the exact
  production-project trust condition, and
  [GCP Workload Identity Federation](https://docs.cloud.google.com/iam/docs/workload-identity-federation)
  for keyless service-account impersonation.
- [Cloud SQL IAM database authentication](https://docs.cloud.google.com/sql/docs/postgres/iam-authentication)
  for login roles, instance flags, database users, and database-level grants.
- [Cloud SQL IAM Conditions](https://docs.cloud.google.com/sql/docs/postgres/iam-conditions)
  for instance-scoped role bindings, and
  [Cloud SQL TLS identity verification](https://docs.cloud.google.com/sql/docs/postgres/configure-ssl-instance)
  for CA-mode and DNS requirements.
