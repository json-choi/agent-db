# DopeDB Workspace Cloud

This is the authenticated web and API control plane for DopeDB workspaces. It is a
separate Next.js application intended for its own Vercel project at `app.dopedb.dev`;
the marketing `site/` deployment remains independent.

## Local setup

Copy `.env.example` to the ignored `workspace-cloud/.env.local` and provide the Neon
pooler/unpooled URLs, Google OAuth web client credentials, a Better Auth secret, and the
exact Better Auth URL. To deliver invitation email, also set `RESEND_API_KEY` and a
verified `WORKSPACE_INVITATION_FROM` sender; without them, the dashboard keeps the
email-bound copy-link fallback. Configure this Google redirect URI:

```text
http://localhost:3000/api/auth/callback/google
```

Then run `pnpm install` in this directory and `pnpm workspace:cloud:dev` from the repo
root. Generate/check migrations with `pnpm db:generate` and `pnpm db:check` here; apply
them through the unpooled URL with `pnpm workspace:migrate` from the repository root.

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
- Target-database passwords and provider API credentials never enter this service.
- Shared connection rows contain only endpoint metadata and safety defaults. Usernames,
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
