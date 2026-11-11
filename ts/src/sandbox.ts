/**
 * NEAR Sandbox harness for the InvoiceGuard integration suite.
 *
 * Starts an isolated `neard` sandbox (near-sandbox 0.3.5 -> neard 2.14.0),
 * creates the full cast of accounts, deploys the compiled WASM, and exposes
 * typed helpers for change/view calls. All financial values are `bigint` or
 * decimal strings; JavaScript `Number` is never used for money.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

import nacl from "tweetnacl";
import bs58 from "bs58";
import {
  Account,
  KeyPair,
  Near,
  keyStores,
  providers,
} from "near-api-js";
import {
  DEFAULT_ACCOUNT_ID,
  DEFAULT_PRIVATE_KEY,
  Sandbox,
} from "near-sandbox";

import {
  PROTOCOL_ID,
  PROTOCOL_VERSION,
  YOCTO_PER_NEAR,
  canonicalBytes,
  makeInvoice,
  nearToYocto,
  type SignedInvoice,
} from "./borsh.ts";
import {
  signInvoice,
  toBase64,
  vendorKeyFromByte,
  type VendorKey,
} from "./keys.ts";

export const TGAS = 10n ** 12n;
/** Gas attached to every change call (enough for transfer + 30 Tgas callback). */
export const CALL_GAS = 100n * TGAS;

const ROOT = DEFAULT_ACCOUNT_ID; // "sandbox"
export const IDS = {
  root: ROOT,
  admin: `admin.${ROOT}`,
  agent: `agent.${ROOT}`,
  vendor: `vendor.${ROOT}`,
  vendor2: `vendor2.${ROOT}`,
  payout: `payout.${ROOT}`,
  payout2: `payout2.${ROOT}`,
  attacker: `attacker.${ROOT}`,
  contract: `guard.${ROOT}`,
  /** Valid named account that is deliberately never created. */
  ghost: `ghost.${ROOT}`,
} as const;

export const NETWORK_ID = "sandbox";

export interface Accounts {
  root: Account;
  admin: Account;
  agent: Account;
  vendor: Account;
  vendor2: Account;
  payout: Account;
  payout2: Account;
  attacker: Account;
  contract: Account;
}

export interface Harness {
  sandbox: Sandbox;
  rpcUrl: string;
  near: Near;
  keyStore: keyStores.InMemoryKeyStore;
  accounts: Accounts;
  keys: Record<string, KeyPair>;
  vendorKey: VendorKey;
  vendor2Key: VendorKey;
  contractId: string;
}

/** Deterministic full-access key pair from a 32-byte seed. */
export function nearKeyPairFromSeed(seed: Uint8Array): KeyPair {
  const pair = nacl.sign.keyPair.fromSeed(seed);
  return KeyPair.fromString(
    `ed25519:${bs58.encode(Uint8Array.from(pair.secretKey))}`,
  );
}

function seedFor(name: string): Uint8Array {
  const seed = new Uint8Array(32);
  const digest = nacl.hash(new TextEncoder().encode(`invoiceguard:${name}`));
  seed.set(digest.subarray(0, 32));
  return seed;
}

/** Minimal JSON-RPC helper (used for block timestamps and raw access keys). */
export async function rpc<T = any>(
  rpcUrl: string,
  method: string,
  params: unknown,
): Promise<T> {
  const response = await fetch(rpcUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: "invoiceguard", method, params }),
  });
  const body: any = await response.json();
  if (body.error) {
    throw new Error(`${method} RPC error: ${JSON.stringify(body.error)}`);
  }
  return body.result as T;
}

/** Current sandbox block time in Unix seconds (UTC). */
export async function blockTimestampSec(rpcUrl: string): Promise<bigint> {
  const block = await rpc<{ header: { timestamp_nanosec: string } }>(
    rpcUrl,
    "block",
    { finality: "final" },
  );
  return BigInt(block.header.timestamp_nanosec) / 1_000_000_000n;
}

/** Human-readable yoctoNEAR amount -> decimal string for JSON args. */
export function yoctoString(amount: bigint): string {
  return amount.toString();
}

/**
 * Convert a small non-financial u64 (nonce, timestamp, request id, ...) to a
 * JSON number. Rust `u64` fields do NOT accept JSON strings. We assert the
 * value fits in a safe integer first; financial values are never converted
 * to `Number` (they use `U128` decimal strings instead).
 */
export function u64ToJson(value: bigint): number {
  if (value < 0n || value > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new RangeError(`value does not fit a safe JSON number: ${value}`);
  }
  return Number(value);
}

/** Guard against accidentally passing a `bigint` (JSON.stringify would throw). */
function assertNoBigInt(args: Record<string, unknown>, path = "args"): void {
  for (const [key, value] of Object.entries(args)) {
    const where = `${path}.${key}`;
    if (typeof value === "bigint") {
      throw new TypeError(`${where} is a bigint; use a decimal string (U128) or u64ToJson()`);
    } else if (value && typeof value === "object" && !Array.isArray(value)) {
      assertNoBigInt(value as Record<string, unknown>, where);
    }
  }
}

export async function startHarness(): Promise<Harness> {
  const wasmPath = path.join(
    path.dirname(fileURLToPath(import.meta.url)),
    "..",
    "..",
    "target",
    "wasm32-unknown-unknown",
    "release",
    "invoiceguard.wasm",
  );
  const wasm = new Uint8Array(readFileSync(wasmPath));

  const sandbox = await Sandbox.start({});
  const rpcUrl = sandbox.rpcUrl;
  const keyStore = new keyStores.InMemoryKeyStore();
  const near = new Near({ networkId: NETWORK_ID, nodeUrl: rpcUrl, keyStore });

  const names = [
    "root",
    "admin",
    "agent",
    "vendor",
    "vendor2",
    "payout",
    "payout2",
    "attacker",
    "contract",
  ] as const;

  const keys: Record<string, KeyPair> = {};
  keys.root = KeyPair.fromString(DEFAULT_PRIVATE_KEY);
  for (const name of names) {
    if (name !== "root") keys[name] = nearKeyPairFromSeed(seedFor(name));
    await keyStore.setKey(NETWORK_ID, IDS[name], keys[name]);
  }

  const root = await near.account(IDS.root);
  // Generous balances: accounts must cover gas plus large attached deposits
  // when funding the treasury, and the contract must hold its own stake plus
  // the funds it will transfer out.
  const accountBalance = 500n * YOCTO_PER_NEAR;
  for (const name of names) {
    if (name === "root") continue;
    if (name === "contract") {
      await root.createAndDeployContract(
        IDS.contract,
        keys.contract.getPublicKey(),
        wasm,
        300n * YOCTO_PER_NEAR,
      );
    } else {
      await root.createAccount(
        IDS[name],
        keys[name].getPublicKey(),
        accountBalance,
      );
    }
  }

  const accounts: Accounts = {
    root,
    admin: await near.account(IDS.admin),
    agent: await near.account(IDS.agent),
    vendor: await near.account(IDS.vendor),
    vendor2: await near.account(IDS.vendor2),
    payout: await near.account(IDS.payout),
    payout2: await near.account(IDS.payout2),
    attacker: await near.account(IDS.attacker),
    contract: await near.account(IDS.contract),
  };

  // Initialize the contract as the administrator.
  await accounts.admin.functionCall({
    contractId: IDS.contract,
    methodName: "new",
    args: {
      admin: IDS.admin,
      agent: IDS.agent,
      network_id: NETWORK_ID,
      max_payment: yoctoString(5n * YOCTO_PER_NEAR),
      daily_limit: yoctoString(20n * YOCTO_PER_NEAR),
      reserve_floor: yoctoString(2n * YOCTO_PER_NEAR),
    },
    gas: CALL_GAS,
  });

  return {
    sandbox,
    rpcUrl,
    near,
    keyStore,
    accounts,
    keys,
    vendorKey: vendorKeyFromByte(1),
    vendor2Key: vendorKeyFromByte(2),
    contractId: IDS.contract,
  };
}

// ---------------------------------------------------------------------------
// Call helpers
// ---------------------------------------------------------------------------

export async function view<T = any>(
  h: Harness,
  methodName: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  return (await h.accounts.root.viewFunction({
    contractId: h.contractId,
    methodName,
    args,
  })) as T;
}

export async function change(
  caller: Account,
  contractId: string,
  methodName: string,
  args: Record<string, unknown> = {},
  attachedDeposit = 0n,
) {
  assertNoBigInt(args);
  return caller.functionCall({
    contractId,
    methodName,
    args,
    gas: CALL_GAS,
    attachedDeposit,
  });
}

/** The contract's JSON return value from a change call (null for Promise returns). */
export function lastResult(outcome: any): any {
  return providers.getTransactionLastResult(outcome);
}

/**
 * Assert that a change call fails. Returns the thrown error so the caller can
 * additionally check the contract's error code.
 */
export async function expectReject(
  label: string,
  fn: () => Promise<unknown>,
  expectedCode?: string,
): Promise<Error> {
  let threw: unknown;
  try {
    await fn();
  } catch (error) {
    threw = error;
  }
  if (!threw) {
    throw new Error(`[${label}] expected the transaction to be rejected, but it succeeded`);
  }
  const message = threw instanceof Error ? threw.message : String(threw);
  if (expectedCode && !message.includes(expectedCode)) {
    throw new Error(
      `[${label}] expected rejection containing "${expectedCode}", got: ${message}`,
    );
  }
  return threw instanceof Error ? threw : new Error(message);
}

/** Read an account's total balance in yoctoNEAR. */
export async function balanceYocto(account: Account): Promise<bigint> {
  const b = await account.getAccountBalance();
  return BigInt(b.total);
}

// ---------------------------------------------------------------------------
// Invoice helpers
// ---------------------------------------------------------------------------

export interface InvoiceOptions {
  vendorId: string;
  nonce: bigint;
  recipient: string;
  amount: bigint;
  expirationUnixSec: bigint;
  protocolId?: string;
  protocolVersion?: number;
  networkId?: string;
  contractId?: string;
}

export function buildInvoice(h: Harness, opts: InvoiceOptions): SignedInvoice {
  return makeInvoice({
    protocol_id: opts.protocolId ?? PROTOCOL_ID,
    protocol_version: opts.protocolVersion ?? PROTOCOL_VERSION,
    network_id: opts.networkId ?? NETWORK_ID,
    contract_id: opts.contractId ?? h.contractId,
    vendor_id: opts.vendorId,
    nonce: opts.nonce,
    recipient: opts.recipient,
    amount: opts.amount,
    expiration_unix_sec: opts.expirationUnixSec,
  });
}

/** Canonical bytes + base64 signature tuple accepted by `pay_invoice`. */
export function signedArgs(invoice: SignedInvoice, key: VendorKey) {
  const message = canonicalBytes(invoice);
  return {
    invoice: {
      protocol_id: invoice.protocol_id,
      // u32/u64 JSON fields must be numbers (Rust serde rejects strings).
      protocol_version: invoice.protocol_version,
      network_id: invoice.network_id,
      contract_id: invoice.contract_id,
      vendor_id: invoice.vendor_id,
      nonce: u64ToJson(invoice.nonce),
      recipient: invoice.recipient,
      // U128 is a decimal string; never a JS Number.
      amount: invoice.amount.toString(),
      expiration_unix_sec: u64ToJson(invoice.expiration_unix_sec),
    },
    signature: toBase64(signInvoice(key, message)),
  };
}

export async function defaultExpiry(h: Harness, seconds = 3600n): Promise<bigint> {
  return (await blockTimestampSec(h.rpcUrl)) + seconds;
}

export { nearToYocto, YOCTO_PER_NEAR };
