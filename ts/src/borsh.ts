/**
 * Canonical Borsh serialization of an InvoiceGuard `SignedInvoice`.
 *
 * This file is the TypeScript half of the cross-language contract. Its byte
 * layout MUST stay in lockstep with `contract/src/invoice.rs`:
 *
 * | # | field                  | type        | Borsh encoding                    |
 * |---|------------------------|-------------|-----------------------------------|
 * | 1 | `protocol_id`          | `String`    | u32 LE length + UTF-8 bytes       |
 * | 2 | `protocol_version`     | `u32`       | 4 bytes LE                        |
 * | 3 | `network_id`           | `String`    | u32 LE length + UTF-8 bytes       |
 * | 4 | `contract_id`          | `AccountId` | u32 LE length + UTF-8 bytes       |
 * | 5 | `vendor_id`            | `AccountId` | u32 LE length + UTF-8 bytes       |
 * | 6 | `nonce`                | `u64`       | 8 bytes LE                        |
 * | 7 | `recipient`            | `AccountId` | u32 LE length + UTF-8 bytes       |
 * | 8 | `amount`               | `u128`      | 16 bytes LE (yoctoNEAR)           |
 * | 9 | `expiration_unix_sec`  | `u64`       | 8 bytes LE (Unix seconds, UTC)    |
 *
 * `AccountId` has the same Borsh layout as `String`. All integers are
 * supplied as `bigint`; JavaScript `Number` is never used for financial
 * values.
 */

export const PROTOCOL_ID = "invoiceguard";
export const PROTOCOL_VERSION = 1;

/** One NEAR in yoctoNEAR (10^24). */
export const YOCTO_PER_NEAR = 10n ** 24n;

/** The vendor-signed invoice. All fields are covered by the signature. */
export interface SignedInvoice {
  protocol_id: string;
  protocol_version: number;
  network_id: string;
  contract_id: string;
  vendor_id: string;
  nonce: bigint;
  recipient: string;
  /** yoctoNEAR */
  amount: bigint;
  /** Unix seconds (UTC), exclusive deadline */
  expiration_unix_sec: bigint;
}

const U32_MAX = (1n << 32n) - 1n;
const U64_MAX = (1n << 64n) - 1n;
const U128_MAX = (1n << 128n) - 1n;

function assertUnsigned(value: bigint, max: bigint, label: string): void {
  if (value < 0n || value > max) {
    throw new RangeError(`${label} out of range: ${value}`);
  }
}

function pushUIntLE(out: number[], value: bigint, byteLength: number, label: string): void {
  assertUnsigned(value, (1n << BigInt(byteLength * 8)) - 1n, label);
  let v = value;
  for (let i = 0; i < byteLength; i++) {
    out.push(Number(v & 0xffn));
    v >>= 8n;
  }
}

function pushString(out: number[], value: string, label: string): void {
  const bytes = new TextEncoder().encode(value);
  if (bytes.length > Number(U32_MAX)) {
    throw new RangeError(`${label} is too long to Borsh-encode`);
  }
  pushUIntLE(out, BigInt(bytes.length), 4, `${label} length`);
  for (const b of bytes) out.push(b);
}

/** Deterministic Borsh encoding of the exact bytes a vendor signs. */
export function canonicalBytes(invoice: SignedInvoice): Uint8Array {
  assertUnsigned(BigInt(invoice.protocol_version), U32_MAX, "protocol_version");
  assertUnsigned(invoice.nonce, U64_MAX, "nonce");
  assertUnsigned(invoice.amount, U128_MAX, "amount");
  assertUnsigned(invoice.expiration_unix_sec, U64_MAX, "expiration_unix_sec");

  const out: number[] = [];
  pushString(out, invoice.protocol_id, "protocol_id");
  pushUIntLE(out, BigInt(invoice.protocol_version), 4, "protocol_version");
  pushString(out, invoice.network_id, "network_id");
  pushString(out, invoice.contract_id, "contract_id");
  pushString(out, invoice.vendor_id, "vendor_id");
  pushUIntLE(out, invoice.nonce, 8, "nonce");
  pushString(out, invoice.recipient, "recipient");
  pushUIntLE(out, invoice.amount, 16, "amount");
  pushUIntLE(out, invoice.expiration_unix_sec, 8, "expiration_unix_sec");
  return Uint8Array.from(out);
}

/** Lowercase hex encoding (used by the shared test vectors). */
export function toHex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

/** Decode a hex string (test vectors only; validates input). */
export function fromHex(hex: string): Uint8Array {
  if (hex.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(hex)) {
    throw new Error("invalid hex string");
  }
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

/**
 * Convert a human-readable NEAR amount to yoctoNEAR using exact integer
 * arithmetic. Accepts an integer `bigint` or a decimal string such as
 * `"1.5"` / `"0.000000000000000000000001"`. Never uses floating point.
 */
export function nearToYocto(amount: bigint | string): bigint {
  if (typeof amount === "bigint") {
    if (amount < 0n) throw new RangeError("negative amount");
    return amount * YOCTO_PER_NEAR;
  }
  const match = /^(\d+)(?:\.(\d+))?$/.exec(amount);
  if (!match) throw new Error(`invalid NEAR amount: ${amount}`);
  const whole = match[1];
  const frac = (match[2] ?? "").padEnd(24, "0").slice(0, 24);
  if ((match[2] ?? "").length > 24) {
    throw new RangeError(`NEAR amount has more than 24 decimal places: ${amount}`);
  }
  return BigInt(whole) * YOCTO_PER_NEAR + BigInt(frac);
}

/**
 * Mirror of `SignedInvoice::validate_domain` in `contract/src/invoice.rs`.
 * Returns `null` when the invoice targets this deployment, otherwise the
 * same error code the contract produces. Client tooling uses this to refuse
 * signing an invoice bound to another protocol/network/contract.
 */
export function validateDomain(
  invoice: SignedInvoice,
  expectedNetworkId: string,
  expectedContractId: string,
): string | null {
  if (invoice.protocol_id !== PROTOCOL_ID) return "E_DOMAIN_PROTOCOL";
  if (invoice.protocol_version !== PROTOCOL_VERSION) return "E_DOMAIN_VERSION";
  if (invoice.network_id !== expectedNetworkId) return "E_DOMAIN_NETWORK";
  if (invoice.contract_id !== expectedContractId) return "E_DOMAIN_CONTRACT";
  return null;
}

/** Convenience constructor using the protocol constants. */
export function makeInvoice(fields: {
  network_id: string;
  contract_id: string;
  vendor_id: string;
  nonce: bigint;
  recipient: string;
  amount: bigint;
  expiration_unix_sec: bigint;
  protocol_id?: string;
  protocol_version?: number;
}): SignedInvoice {
  return {
    protocol_id: fields.protocol_id ?? PROTOCOL_ID,
    protocol_version: fields.protocol_version ?? PROTOCOL_VERSION,
    network_id: fields.network_id,
    contract_id: fields.contract_id,
    vendor_id: fields.vendor_id,
    nonce: fields.nonce,
    recipient: fields.recipient,
    amount: fields.amount,
    expiration_unix_sec: fields.expiration_unix_sec,
  };
}
