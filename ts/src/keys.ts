/**
 * Ed25519 vendor keys and signatures for InvoiceGuard.
 *
 * Vendors sign the canonical Borsh payload with a raw Ed25519 key. The public
 * half is registered on-chain as a NEAR `ed25519:<base58>` key string, and
 * the signature is passed as a base64 string (near-sdk `Base64VecU8`).
 */

import nacl from "tweetnacl";
import bs58 from "bs58";

export interface VendorKey {
  /** 32-byte Ed25519 seed. */
  readonly seed: Uint8Array;
  /** 32-byte Ed25519 public key. */
  readonly publicKey: Uint8Array;
  /** 64-byte tweetnacl secret key (seed || public key). */
  readonly secretKey: Uint8Array;
  /** `ed25519:<base58(publicKey)>`, accepted by `register_vendor`. */
  readonly nearPublicKey: string;
}

/** Derive a deterministic vendor key pair from a 32-byte seed. */
export function vendorKeyFromSeed(seed: Uint8Array): VendorKey {
  if (seed.length !== 32) {
    throw new Error("Ed25519 seed must be exactly 32 bytes");
  }
  const pair = nacl.sign.keyPair.fromSeed(seed);
  return {
    seed: Uint8Array.from(seed),
    publicKey: Uint8Array.from(pair.publicKey),
    secretKey: Uint8Array.from(pair.secretKey),
    nearPublicKey: `ed25519:${bs58.encode(Uint8Array.from(pair.publicKey))}`,
  };
}

/** A vendor key from a fixed 32-byte pattern (deterministic test keys). */
export function vendorKeyFromByte(byte: number): VendorKey {
  return vendorKeyFromSeed(new Uint8Array(32).fill(byte & 0xff));
}

/** Sign the canonical payload. Ed25519 hashes the message internally. */
export function signInvoice(key: VendorKey, message: Uint8Array): Uint8Array {
  return Uint8Array.from(nacl.sign.detached(message, key.secretKey));
}

/** Verify an Ed25519 signature over a message. */
export function verifySignature(
  message: Uint8Array,
  signature: Uint8Array,
  publicKey: Uint8Array,
): boolean {
  if (signature.length !== 64 || publicKey.length !== 32) return false;
  return nacl.sign.detached.verify(message, signature, publicKey);
}

export function toBase64(bytes: Uint8Array): string {
  return Buffer.from(bytes).toString("base64");
}

export function fromBase64(value: string): Uint8Array {
  return Uint8Array.from(Buffer.from(value, "base64"));
}

/** Stable hex helpers (test vectors / debugging). */
export function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

export function hexToBytes(hex: string): Uint8Array {
  const clean = hex.startsWith("0x") ? hex.slice(2) : hex;
  if (clean.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(clean)) {
    throw new Error("invalid hex string");
  }
  const out = new Uint8Array(clean.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}
