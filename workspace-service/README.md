# Workspace control plane

The workspace service is a separate trust boundary from the Tauri desktop app. Its
PostgreSQL database currently stores identity, membership, audit metadata, and redacted
shared connection templates. It never stores target-database usernames or passwords,
provider API tokens, certificates, MCP tokens, connection URLs, advanced connection
parameters, or ordinary query result rows.

## Database

Set `DATABASE_URL_UNPOOLED` in the ignored root `.env.local`, then run:

```sh
pnpm workspace:migrate
```

The command runs Drizzle Kit with the code-first schema at
`workspace-cloud/lib/schema.ts` and versioned migrations under
`workspace-cloud/drizzle/`. Runtime API traffic uses the Better Auth Drizzle adapter and
Drizzle ORM over the Neon pooler URL.

The schema is isolated under `workspace_control`. Better Auth owns users, accounts,
sessions, organizations, invitations, and RFC 8628 device codes. Provider OAuth token
fields are forcibly cleared by database hooks before account writes. The API process
lives in `workspace-cloud/`; it must resolve `encryption_key_ref` through KMS before
encrypted shared resources are enabled.
