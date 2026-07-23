// Small AES-256-GCM envelope primitive. Callers supply a decoded key and bind each
// ciphertext to a stable record id through authenticated additional data.
import {
  createCipheriv,
  createDecipheriv,
  randomBytes,
} from "node:crypto";

const VERSION = "v1";
const NONCE_BYTES = 12;
const TAG_BYTES = 16;

export function decodeEnvelopeKey(encoded: string): Buffer {
  if (!/^[A-Za-z0-9_-]{43}$/.test(encoded)) {
    throw new Error("Credential key must be canonical unpadded base64url");
  }
  const decoded = Buffer.from(encoded, "base64url");
  if (decoded.length !== 32 || decoded.toString("base64url") !== encoded) {
    throw new Error("Credential key must decode to exactly 32 bytes");
  }
  return decoded;
}

export function sealEnvelope(key: Buffer, plaintext: string, context: string): string {
  if (key.length !== 32) throw new Error("Credential key must be exactly 32 bytes");
  const nonce = randomBytes(NONCE_BYTES);
  const cipher = createCipheriv("aes-256-gcm", key, nonce);
  cipher.setAAD(Buffer.from(context, "utf8"));
  const ciphertext = Buffer.concat([
    cipher.update(plaintext, "utf8"),
    cipher.final(),
  ]);
  const tag = cipher.getAuthTag();
  return [
    VERSION,
    nonce.toString("base64url"),
    ciphertext.toString("base64url"),
    tag.toString("base64url"),
  ].join(".");
}

export function openEnvelope(key: Buffer, envelope: string, context: string): string {
  if (key.length !== 32) throw new Error("Credential key must be exactly 32 bytes");
  const parts = envelope.split(".");
  if (parts.length !== 4 || parts[0] !== VERSION) {
    throw new Error("Unsupported credential envelope");
  }
  const nonce = Buffer.from(parts[1], "base64url");
  const ciphertext = Buffer.from(parts[2], "base64url");
  const tag = Buffer.from(parts[3], "base64url");
  if (nonce.length !== NONCE_BYTES || tag.length !== TAG_BYTES) {
    throw new Error("Invalid credential envelope");
  }
  const decipher = createDecipheriv("aes-256-gcm", key, nonce);
  decipher.setAAD(Buffer.from(context, "utf8"));
  decipher.setAuthTag(tag);
  return Buffer.concat([
    decipher.update(ciphertext),
    decipher.final(),
  ]).toString("utf8");
}
