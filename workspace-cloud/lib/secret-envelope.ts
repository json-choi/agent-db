// Server-only wrapper around the envelope primitive. The key is separated from
// database ciphertext in the deployment environment and never returned by APIs.
import "server-only";

import { env } from "./env";
import {
  decodeEnvelopeKey,
  openEnvelope,
  sealEnvelope,
} from "./secret-envelope-core";

function key(): Buffer {
  return decodeEnvelopeKey(env.credentialKey());
}

function context(integrationId: string) {
  return `dopedb:provider-integration:${integrationId}`;
}

export function sealProviderCredential(integrationId: string, value: unknown): string {
  return sealEnvelope(key(), JSON.stringify(value), context(integrationId));
}

export function openProviderCredential<T>(integrationId: string, envelope: string): T {
  const plaintext = openEnvelope(key(), envelope, context(integrationId));
  try {
    return JSON.parse(plaintext) as T;
  } finally {
    // JavaScript strings cannot be reliably zeroized. Keep the plaintext lifetime
    // inside this narrow function and never retain or log it outside typed callers.
  }
}
