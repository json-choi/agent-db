// Provider-neutral contracts for redacted resource discovery and one-time database
// leases. Secret-bearing adapters narrow external responses into these shapes.

export type ManagedEngine = "postgres" | "mysql";
export type ManagedAccessMode = "read" | "write";
export type ManagedSslMode = "verify-ca" | "verify-full";

export type ProviderResourceItem = {
  id: string;
  name: string;
  value: string;
  kind?: ManagedEngine;
  production?: boolean;
  ready?: boolean;
};

export type ManagedProviderLease = {
  externalCredentialId: string;
  externalCredentialKind: "iamToken" | "password" | "role";
  host: string;
  port: number;
  database: string;
  username: string;
  password: string;
  sslmode: ManagedSslMode;
  tlsServerCaPem?: string;
  expiresAt: string;
};

export class ProviderRequestError extends Error {
  constructor(
    public readonly provider: string,
    message: string,
    public readonly status: number,
  ) {
    super(message);
    this.name = "ProviderRequestError";
  }
}
