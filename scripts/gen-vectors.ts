/**
 * Generate `tests/vectors.json`: shared Rust <-> TypeScript test vectors.
 *
 * Deterministic: fixed seeds, fixed field values, no randomness. The Rust
 * unit test (`contract/src/tests/invoice_tests.rs`) reads the same file and
 * asserts byte-for-byte equality with its own Borsh implementation and
 * verifies every signature with `ed25519-dalek`.
 *
 * Run with: npm run vectors
 */

import { mkdirSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

import {
  PROTOCOL_ID,
  PROTOCOL_VERSION,
  canonicalBytes,
  makeInvoice,
  toHex,
  type SignedInvoice,
} from "../ts/src/borsh.ts";
import {
  bytesToHex,
  signInvoice,
  toBase64,
  vendorKeyFromByte,
} from "../ts/src/keys.ts";

const here = path.dirname(fileURLToPath(import.meta.url));
const outPath = path.join(here, "..", "tests", "vectors.json");

const NETWORK_ID = "sandbox";
const CONTRACT_ID = "invoiceguard.unit.test.near";
const VENDOR_ID = "vendor.unit.test.near";
const RECIPIENT = "payout.unit.test.near";

const key = vendorKeyFromByte(1);
const referenceSeedHex = bytesToHex(key.seed);

const base = {
  network_id: NETWORK_ID,
  contract_id: CONTRACT_ID,
  vendor_id: VENDOR_ID,
  nonce: 7n,
  recipient: RECIPIENT,
  amount: 42_424_242_424_242_424_242n,
  expiration_unix_sec: 1_893_456_000n,
};

interface Case {
  name: string;
  invoice: SignedInvoice;
  /** Expected result of `validate_domain("sandbox", contract_id)`. */
  domain_error: string | null;
}

const cases: Case[] = [
  {
    name: "valid",
    invoice: makeInvoice(base),
    domain_error: null,
  },
  {
    name: "changed_amount",
    invoice: makeInvoice({ ...base, amount: base.amount + 1n }),
    domain_error: null,
  },
  {
    name: "changed_recipient",
    invoice: makeInvoice({ ...base, recipient: "payout2.unit.test.near" }),
    domain_error: null,
  },
  {
    name: "changed_network",
    invoice: makeInvoice({ ...base, network_id: "testnet" }),
    domain_error: "E_DOMAIN_NETWORK",
  },
  {
    name: "changed_contract",
    invoice: makeInvoice({ ...base, contract_id: "other.unit.test.near" }),
    domain_error: "E_DOMAIN_CONTRACT",
  },
];

/** Big integers are stored as decimal strings to avoid JSON precision loss. */
function serializeInvoice(inv: SignedInvoice) {
  return {
    protocol_id: inv.protocol_id,
    protocol_version: inv.protocol_version,
    network_id: inv.network_id,
    contract_id: inv.contract_id,
    vendor_id: inv.vendor_id,
    nonce: inv.nonce.toString(),
    recipient: inv.recipient,
    amount: inv.amount.toString(),
    expiration_unix_sec: inv.expiration_unix_sec.toString(),
  };
}

const validBytes = canonicalBytes(cases[0].invoice);
const referenceSignature = signInvoice(key, validBytes);

const vectors = {
  description:
    "Shared InvoiceGuard canonical-Borsh / Ed25519 vectors. All integers are decimal strings.",
  protocol_id: PROTOCOL_ID,
  protocol_version: PROTOCOL_VERSION,
  network_id: NETWORK_ID,
  contract_id: CONTRACT_ID,
  signing_seed_hex: referenceSeedHex,
  public_key: key.nearPublicKey,
  public_key_hex: bytesToHex(key.publicKey),
  /** Signature over the `valid` case; must NOT verify for any changed case. */
  reference_signature_hex: bytesToHex(referenceSignature),
  reference_signature_base64: toBase64(referenceSignature),
  cases: cases.map((c) => {
    const bytes = canonicalBytes(c.invoice);
    const sig = signInvoice(key, bytes);
    return {
      name: c.name,
      invoice: serializeInvoice(c.invoice),
      canonical_hex: toHex(bytes),
      signature_hex: bytesToHex(sig),
      signature_base64: toBase64(sig),
      domain_error: c.domain_error,
    };
  }),
};

mkdirSync(path.dirname(outPath), { recursive: true });
writeFileSync(outPath, `${JSON.stringify(vectors, null, 2)}\n`, "utf8");
console.log(`Wrote ${vectors.cases.length} vectors to ${outPath}`);
for (const c of vectors.cases) {
  console.log(`  ${c.name.padEnd(20)} ${c.canonical_hex}`);
}
