/**
 * InvoiceGuard NEAR Sandbox integration suite (CP1).
 *
 * Runs against a real `neard` sandbox with the compiled WASM. Every check
 * below is an actual transaction or view call; nothing is faked. Covers:
 * deployment/init, vendor registry, valid payment + real balance change,
 * forged/tampered/redirected/replayed/expired/domain-separated invoices,
 * limits, failed transfer accounting, private callback, manual approval,
 * and a restricted function-call access key.
 *
 * Run with: npm run test:sandbox
 */

import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

import { Near, keyStores, type Account } from "near-api-js";

import {
  IDS,
  NETWORK_ID,
  YOCTO_PER_NEAR,
  balanceYocto,
  blockTimestampSec,
  buildInvoice,
  change,
  defaultExpiry,
  expectReject,
  lastResult,
  nearKeyPairFromSeed,
  signedArgs,
  startHarness,
  u64ToJson,
  view,
  type Harness,
} from "../ts/src/sandbox.ts";
import {
  PROTOCOL_ID,
  PROTOCOL_VERSION,
  type SignedInvoice,
} from "../ts/src/borsh.ts";
import { vendorKeyFromByte, type VendorKey } from "../ts/src/keys.ts";

const NEAR = YOCTO_PER_NEAR;
const ONE = 1n * NEAR;

// ---------------------------------------------------------------------------
// Tiny test runner (sequential; all checks share the same deployed contract)
// ---------------------------------------------------------------------------

interface Result {
  name: string;
  ok: boolean;
  detail?: string;
}
const results: Result[] = [];

async function test(name: string, fn: () => Promise<void>): Promise<void> {
  try {
    await fn();
    results.push({ name, ok: true });
    console.log(`  PASS  ${name}`);
  } catch (error) {
    const detail = error instanceof Error ? error.stack ?? error.message : String(error);
    results.push({ name, ok: false, detail });
    console.error(`  FAIL  ${name}\n${detail}`);
  }
}

function check(condition: unknown, message: string): asserts condition {
  assert.ok(condition, message);
}

/** Collect all contract logs from a final execution outcome. */
function outcomeLogs(outcome: any): string[] {
  const logs: string[] = [];
  const push = (o: any) => {
    if (Array.isArray(o?.outcome?.logs)) logs.push(...o.outcome.logs);
  };
  push(outcome?.transaction_outcome);
  for (const r of outcome?.receipts_outcome ?? []) push(r);
  return logs;
}

// ---------------------------------------------------------------------------
// Shared state helpers
// ---------------------------------------------------------------------------

let h: Harness;
let expectedLedger: bigint;
let expectedSpent: bigint;

async function config() {
  return view<any>(h, "get_config");
}
async function treasury() {
  return view<any>(h, "get_treasury");
}
async function vendorNonce(vendorId: string = IDS.vendor): Promise<bigint> {
  const v = await view<any>(h, "get_vendor", { vendor_id: vendorId });
  check(v, `vendor ${vendorId} exists`);
  return BigInt(v.next_nonce);
}
async function nextPaymentId(): Promise<bigint> {
  return BigInt((await config()).next_payment_id);
}
async function payment(id: bigint) {
  return view<any>(h, "get_payment", { payment_id: u64ToJson(id) });
}
async function payoutBalance(): Promise<bigint> {
  return balanceYocto(h.accounts.payout);
}

async function submitInvoice(
  invoice: SignedInvoice,
  key: VendorKey,
  caller: Account = h.accounts.agent,
) {
  return change(caller, h.contractId, "pay_invoice", signedArgs(invoice, key));
}

/** A fresh, correctly-domain-separated invoice for the main vendor. */
async function mainInvoice(fields: {
  nonce: bigint;
  amount: bigint;
  recipient?: string;
  expiration?: bigint;
  vendorId?: string;
  networkId?: string;
  contractId?: string;
  protocolId?: string;
  protocolVersion?: number;
}): Promise<SignedInvoice> {
  return buildInvoice(h, {
    vendorId: fields.vendorId ?? IDS.vendor,
    nonce: fields.nonce,
    recipient: fields.recipient ?? IDS.payout,
    amount: fields.amount,
    expirationUnixSec: fields.expiration ?? (await defaultExpiry(h)),
    networkId: fields.networkId,
    contractId: fields.contractId,
    protocolId: fields.protocolId,
    protocolVersion: fields.protocolVersion,
  });
}

async function assertAccounting(label: string): Promise<void> {
  const t = await treasury();
  check(
    BigInt(t.ledger_balance) === expectedLedger,
    `${label}: ledger ${t.ledger_balance} != ${expectedLedger}`,
  );
  check(BigInt(t.total_reserved) === 0n, `${label}: reservation not released`);
  check(
    BigInt(t.today_spent) === expectedSpent,
    `${label}: today_spent ${t.today_spent} != ${expectedSpent}`,
  );
  check(BigInt(t.today_reserved) === 0n, `${label}: today_reserved != 0`);
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

const wasmNote = fileURLToPath(new URL("../target/wasm32-unknown-unknown/release/invoiceguard.wasm", import.meta.url));

console.log("InvoiceGuard Sandbox integration suite");
console.log(`  WASM: ${wasmNote}`);

h = await startHarness();

try {
  // ---- Setup: register vendors and fund the treasury -------------------
  await change(h.accounts.admin, h.contractId, "register_vendor", {
    vendor_id: IDS.vendor,
    signing_key: h.vendorKey.nearPublicKey,
    payout_account: IDS.payout,
  });
  await change(h.accounts.admin, h.contractId, "fund", {}, 100n * NEAR);
  expectedLedger = 100n * NEAR;
  expectedSpent = 0n;

  // Vendor 2 pays to a valid named account that is never created: used to
  // force a real failed transfer (not an implicit 64-hex account).
  await change(h.accounts.admin, h.contractId, "register_vendor", {
    vendor_id: IDS.vendor2,
    signing_key: h.vendor2Key.nearPublicKey,
    payout_account: IDS.ghost,
  });

  await test("T01 deploy/init: config and invoice domain are correct", async () => {
    const c = await config();
    check(c.admin === IDS.admin, "admin configured");
    check(c.agent === IDS.agent, "agent configured");
    check(c.network_id === NETWORK_ID, "network id");
    check(c.paused === false, "not paused");
    check(BigInt(c.max_payment) === 5n * NEAR, "max payment 5 NEAR");
    check(BigInt(c.daily_limit) === 20n * NEAR, "daily limit 20 NEAR");
    check(BigInt(c.reserve_floor) === 2n * NEAR, "reserve floor 2 NEAR");
    check(BigInt(c.request_ttl_sec) === 86400n, "default TTL 24h");
    check(BigInt(c.vendors) === 2n, "both setup vendors registered");
    const d = await view<any>(h, "get_invoice_domain");
    check(d.protocol_id === PROTOCOL_ID, "protocol id");
    check(d.protocol_version === PROTOCOL_VERSION, "protocol version");
    check(d.network_id === NETWORK_ID, "domain network id");
    check(d.contract_id === h.contractId, "domain contract id");
  });

  await test("T02 re-initialization is rejected by #[init]", async () => {
    await expectReject(
      "re-init",
      () =>
        change(h.accounts.admin, h.contractId, "new", {
          admin: IDS.admin,
          agent: IDS.agent,
          network_id: NETWORK_ID,
          max_payment: (5n * NEAR).toString(),
          daily_limit: (20n * NEAR).toString(),
          reserve_floor: (2n * NEAR).toString(),
        }),
      "already been initialized",
    );
  });

  await test("T03 vendor registry: duplicate, non-admin, contract payout", async () => {
    const v = await view<any>(h, "get_vendor", { vendor_id: IDS.vendor });
    check(v.active === true, "vendor active");
    check(BigInt(v.next_nonce) === 1n, "nonce starts at 1");
    check(v.signing_key === h.vendorKey.nearPublicKey, "registered signing key");
    check(v.payout_account === IDS.payout, "registered payout account");

    await expectReject(
      "duplicate vendor",
      () =>
        change(h.accounts.admin, h.contractId, "register_vendor", {
          vendor_id: IDS.vendor,
          signing_key: h.vendorKey.nearPublicKey,
          payout_account: IDS.payout,
        }),
      "E_VENDOR_EXISTS",
    );
    await expectReject(
      "agent cannot register vendor",
      () =>
        change(h.accounts.agent, h.contractId, "register_vendor", {
          vendor_id: IDS.attacker,
          signing_key: h.vendorKey.nearPublicKey,
          payout_account: IDS.attacker,
        }),
      "E_NOT_ADMIN",
    );
    await expectReject(
      "payout cannot be contract",
      () =>
        change(h.accounts.admin, h.contractId, "register_vendor", {
          vendor_id: IDS.attacker,
          signing_key: h.vendorKey.nearPublicKey,
          payout_account: h.contractId,
        }),
      "E_PAYOUT_IS_CONTRACT",
    );
  });

  await test("T04 funding: zero deposit rejected, anyone may fund", async () => {
    await expectReject(
      "zero deposit",
      () => change(h.accounts.admin, h.contractId, "fund", {}, 0n),
      "E_ZERO_DEPOSIT",
    );
    const before = (await treasury()).ledger_balance;
    await change(h.accounts.attacker, h.contractId, "fund", {}, 1n * NEAR);
    const after = (await treasury()).ledger_balance;
    check(BigInt(after) - BigInt(before) === 1n * NEAR, "ledger credited by attached deposit only");
    expectedLedger += 1n * NEAR;
    await assertAccounting("after funding");
  });

  // ---- Valid payment ---------------------------------------------------
  await test("T05 valid signed invoice: transfer, balance change, accounting, events", async () => {
    const balanceBefore = await payoutBalance();
    const nonce = await vendorNonce();
    const invoice = await mainInvoice({ nonce, amount: ONE });
    const outcome = await submitInvoice(invoice, h.vendorKey);
    const id = (await nextPaymentId()) - 1n;

    const p = await payment(id);
    check(p, "payment record exists");
    check(p.status === "paid", `status ${p.status} == paid`);
    check(p.recipient === IDS.payout, "recipient is registered payout");
    check(BigInt(p.amount) === ONE, "amount exact");
    check(p.kind.vendor_invoice?.nonce === Number(nonce), "vendor invoice kind carries nonce");

    const balanceAfter = await payoutBalance();
    check(balanceAfter - balanceBefore === ONE, "payout account received exactly 1 NEAR");

    check(BigInt((await vendorNonce())) === nonce + 1n, "nonce consumed");
    expectedLedger -= ONE;
    expectedSpent += ONE;
    await assertAccounting("after valid payment");

    const logs = outcomeLogs(outcome);
    check(logs.some((l) => l.includes("invoice_accepted")), "invoice_accepted event");
    check(logs.some((l) => l.includes("payment_processing")), "payment_processing event");
    check(logs.some((l) => l.includes("payment_paid")), "payment_paid event");
  });

  // ---- Rejection scenarios --------------------------------------------
  const nonceNow = await vendorNonce();

  await test("T06 forged signature (attacker key) is rejected", async () => {
    const inv = await mainInvoice({ nonce: nonceNow, amount: ONE });
    const attackerKey = vendorKeyFromByte(3);
    await expectReject("forged", () => submitInvoice(inv, attackerKey), "E_BAD_SIGNATURE");
    check((await vendorNonce()) === nonceNow, "nonce unchanged on rejection");
    await assertAccounting("after forged");
  });

  await test("T07 tampered amount is rejected", async () => {
    const idsBefore = await nextPaymentId();
    const signed = await mainInvoice({ nonce: nonceNow, amount: ONE });
    const args = signedArgs(signed, h.vendorKey);
    // Same signature, but the submitted amount is 2 NEAR instead of 1.
    args.invoice.amount = (2n * NEAR).toString();
    await expectReject(
      "tampered amount",
      () => change(h.accounts.agent, h.contractId, "pay_invoice", args),
      "E_BAD_SIGNATURE",
    );
    check((await nextPaymentId()) === idsBefore, "no payment created");
    await assertAccounting("after tampered amount");
  });

  await test("T08 recipient substitution is rejected even with a valid signature", async () => {
    // Simulates a compromised vendor key: the vendor signs an invoice paying
    // the attacker, but the registered payout account still wins.
    const inv = await mainInvoice({ nonce: nonceNow, amount: ONE, recipient: IDS.attacker });
    const args = signedArgs(inv, h.vendorKey);
    await expectReject(
      "recipient substitution",
      () => change(h.accounts.agent, h.contractId, "pay_invoice", args),
      "E_RECIPIENT_MISMATCH",
    );
    await assertAccounting("after recipient substitution");
  });

  await test("T09 replay of a consumed invoice is rejected", async () => {
    const inv = await mainInvoice({ nonce: 1n, amount: ONE });
    await expectReject("replay", () => submitInvoice(inv, h.vendorKey), "E_NONCE_MISMATCH");
    await assertAccounting("after replay");
  });

  await test("T10 wrong nonce is rejected", async () => {
    const inv = await mainInvoice({ nonce: 999n, amount: ONE });
    await expectReject("wrong nonce", () => submitInvoice(inv, h.vendorKey), "E_NONCE_MISMATCH");
  });

  await test("T11 expired invoice is rejected", async () => {
    const now = await blockTimestampSec(h.rpcUrl);
    const inv = await mainInvoice({ nonce: nonceNow, amount: ONE, expiration: now - 60n });
    await expectReject("expired", () => submitInvoice(inv, h.vendorKey), "E_INVOICE_EXPIRED");
  });

  await test("T12 wrong network domain is rejected", async () => {
    const inv = await mainInvoice({ nonce: nonceNow, amount: ONE, networkId: "testnet" });
    await expectReject("wrong network", () => submitInvoice(inv, h.vendorKey), "E_DOMAIN_NETWORK");
  });

  await test("T13 wrong contract domain is rejected", async () => {
    const inv = await mainInvoice({ nonce: nonceNow, amount: ONE, contractId: `other.${IDS.root}` });
    await expectReject("wrong contract", () => submitInvoice(inv, h.vendorKey), "E_DOMAIN_CONTRACT");
  });

  await test("T14 wrong protocol id/version is rejected", async () => {
    const a = await mainInvoice({ nonce: nonceNow, amount: ONE, protocolId: "otherproto" });
    await expectReject("wrong protocol id", () => submitInvoice(a, h.vendorKey), "E_DOMAIN_PROTOCOL");
    const b = await mainInvoice({ nonce: nonceNow, amount: ONE, protocolVersion: 2 });
    await expectReject("wrong version", () => submitInvoice(b, h.vendorKey), "E_DOMAIN_VERSION");
  });

  await test("T15 cross-vendor signature is rejected", async () => {
    const inv = buildInvoice(h, {
      vendorId: IDS.vendor2,
      nonce: 1n,
      recipient: IDS.ghost,
      amount: ONE,
      expirationUnixSec: await defaultExpiry(h),
    });
    // Signed with vendor1's key, but the invoice names vendor2.
    await expectReject("cross-vendor", () => submitInvoice(inv, h.vendorKey), "E_BAD_SIGNATURE");
  });

  await test("T16 inactive vendor cannot be paid", async () => {
    await change(h.accounts.admin, h.contractId, "set_vendor_active", {
      vendor_id: IDS.vendor,
      active: false,
    });
    const inv = await mainInvoice({ nonce: nonceNow, amount: ONE });
    await expectReject("inactive vendor", () => submitInvoice(inv, h.vendorKey), "E_VENDOR_INACTIVE");
    await change(h.accounts.admin, h.contractId, "set_vendor_active", {
      vendor_id: IDS.vendor,
      active: true,
    });
  });

  await test("T17 only the configured agent can submit invoices", async () => {
    const inv = await mainInvoice({ nonce: nonceNow, amount: ONE });
    await expectReject("admin caller", () => submitInvoice(inv, h.vendorKey, h.accounts.admin), "E_NOT_AGENT");
    await expectReject("attacker caller", () => submitInvoice(inv, h.vendorKey, h.accounts.attacker), "E_NOT_AGENT");
  });

  await test("T18 per-payment maximum is enforced", async () => {
    const inv = await mainInvoice({ nonce: nonceNow, amount: 6n * NEAR });
    await expectReject("over max", () => submitInvoice(inv, h.vendorKey), "E_OVER_MAX_PAYMENT");
  });

  await test("T19 daily limit is enforced", async () => {
    await change(h.accounts.admin, h.contractId, "set_max_payment", {
      max_payment: (10n * NEAR).toString(),
    });
    await change(h.accounts.admin, h.contractId, "set_daily_limit", {
      daily_limit: ONE.toString(),
    });
    const inv = await mainInvoice({ nonce: nonceNow, amount: ONE });
    await expectReject("over daily limit", () => submitInvoice(inv, h.vendorKey), "E_OVER_DAILY_LIMIT");
    await change(h.accounts.admin, h.contractId, "set_max_payment", {
      max_payment: (5n * NEAR).toString(),
    });
    await change(h.accounts.admin, h.contractId, "set_daily_limit", {
      daily_limit: (20n * NEAR).toString(),
    });
  });

  // ---- Failed transfer -------------------------------------------------
  await test("T20 failed transfer: reservation released, ledger unchanged, nonce consumed", async () => {
    const balanceBefore = await balanceYocto(h.accounts.payout);
    const nonce = await vendorNonce(IDS.vendor2);
    const inv = buildInvoice(h, {
      vendorId: IDS.vendor2,
      nonce,
      recipient: IDS.ghost,
      amount: ONE,
      expirationUnixSec: await defaultExpiry(h),
    });
    const outcome = await submitInvoice(inv, h.vendor2Key);
    const id = (await nextPaymentId()) - 1n;

    const p = await payment(id);
    check(p.status === "failed", `status ${p.status} == failed`);
    check(BigInt((await vendorNonce(IDS.vendor2))) === nonce + 1n, "nonce stays consumed after failure");
    check((await balanceYocto(h.accounts.payout)) === balanceBefore, "no payout on failure");
    // Ledger and daily spent are unchanged by a failed transfer.
    await assertAccounting("after failed transfer");
    const logs = outcomeLogs(outcome);
    check(logs.some((l) => l.includes("payment_failed")), "payment_failed event");

    const replay = buildInvoice(h, {
      vendorId: IDS.vendor2,
      nonce,
      recipient: IDS.ghost,
      amount: ONE,
      expirationUnixSec: await defaultExpiry(h),
    });
    await expectReject("failed-invoice replay", () => submitInvoice(replay, h.vendor2Key), "E_NONCE_MISMATCH");
  });

  await test("T21 direct invocation of the private callback is rejected", async () => {
    const id = (await nextPaymentId()) - 1n;
    await expectReject(
      "direct callback",
      () =>
        change(h.accounts.admin, h.contractId, "on_transfer_complete", {
          payment_id: u64ToJson(id),
        }),
      "is private",
    );
    await assertAccounting("after direct callback attempt");
  });

  // ---- Manual approval -------------------------------------------------
  await test("T22 manual request: stored fields, admin approval, exact amount", async () => {
    const balanceBefore = await payoutBalance();
    const created = await change(h.accounts.agent, h.contractId, "request_manual_payment", {
      vendor_id: IDS.vendor,
      amount: ONE.toString(),
      memo: "coffee beans",
    });
    const id = BigInt((await config()).next_payment_id) - 1n;
    check(typeof lastResult(created) === "number", "request returns a numeric id");

    const pending = await payment(id);
    check(pending.status === "pending_approval", `status ${pending.status}`);
    check(pending.recipient === null, "recipient bound only at approval");
    check(BigInt(pending.amount) === ONE, "stored amount");
    check(pending.kind.manual_request?.memo === "coffee beans", "stored memo");

    const pendingList = await view<any[]>(h, "get_pending_requests");
    check(pendingList.some((p) => Number(p.id) === Number(id)), "request in pending list");

    await expectReject(
      "agent cannot approve",
      () =>
        change(h.accounts.agent, h.contractId, "approve_manual_payment", {
          request_id: u64ToJson(id),
        }),
      "E_NOT_ADMIN",
    );

    // Approval takes ONLY the request id. Extra fields are ignored by the
    // contract's argument struct: the stored amount/recipient are used.
    const approved = await change(h.accounts.admin, h.contractId, "approve_manual_payment", {
      request_id: u64ToJson(id),
      amount: (999n * NEAR).toString(),
      recipient: IDS.attacker,
    });
    check(approved, "approval transaction succeeded");

    const p = await payment(id);
    check(p.status === "paid", `approved request status ${p.status} == paid`);
    check(p.recipient === IDS.payout, "recipient is registered payout, not attacker");
    check(BigInt(p.amount) === ONE, "paid exactly the stored amount");
    check((await payoutBalance()) - balanceBefore === ONE, "payout received exactly 1 NEAR");

    expectedLedger -= ONE;
    expectedSpent += ONE;
    await assertAccounting("after manual approval");
    const afterList = await view<any[]>(h, "get_pending_requests");
    check(!afterList.some((x) => Number(x.id) === Number(id)), "removed from pending list");
  });

  await test("T23 manual rejection discards the request without moving funds", async () => {
    await change(h.accounts.agent, h.contractId, "request_manual_payment", {
      vendor_id: IDS.vendor,
      amount: ONE.toString(),
      memo: null,
    });
    const id = BigInt((await config()).next_payment_id) - 1n;
    await change(h.accounts.admin, h.contractId, "reject_manual_payment", {
      request_id: u64ToJson(id),
    });
    check((await payment(id)).status === "rejected", "status rejected");
    await assertAccounting("after rejection");
  });

  await test("T24 expired manual request cannot be approved", async () => {
    await change(h.accounts.admin, h.contractId, "set_request_ttl_seconds", {
      ttl_seconds: 0,
    });
    await change(h.accounts.agent, h.contractId, "request_manual_payment", {
      vendor_id: IDS.vendor,
      amount: ONE.toString(),
      memo: "expired",
    });
    const id = BigInt((await config()).next_payment_id) - 1n;
    await expectReject(
      "expired request",
      () =>
        change(h.accounts.admin, h.contractId, "approve_manual_payment", {
          request_id: u64ToJson(id),
        }),
      "E_REQUEST_EXPIRED",
    );
    await change(h.accounts.admin, h.contractId, "set_request_ttl_seconds", {
      ttl_seconds: 86400,
    });
    await assertAccounting("after expired request");
  });

  // ---- Restricted function-call key ------------------------------------
  await test("T25 restricted function-call access key permissions (real transactions)", async () => {
    // Deterministic key, added as a function-call key restricted to this
    // contract and only the two agent methods, with a 1 NEAR gas allowance.
    const fakSeed = new Uint8Array(32).fill(42);
    const fak = nearKeyPairFromSeed(fakSeed);
    const allowance = 1n * NEAR;
    await h.accounts.agent.addKey(
      fak.getPublicKey(),
      h.contractId,
      ["pay_invoice", "request_manual_payment"],
      allowance,
    );

    const allKeys = await h.accounts.agent.getAccessKeys();
    const funk = allKeys.find((k: any) => k.public_key === fak.getPublicKey().toString());
    check(funk, "function-call key present on agent account");
    const perm = (funk as any).access_key.permission;
    check(typeof perm === "object" && perm.FunctionCall, "permission is FunctionCall, not FullAccess");
    const fc = perm.FunctionCall;
    check(fc.receiver_id === h.contractId, "restricted to InvoiceGuard contract");
    check(
      JSON.stringify([...fc.method_names].sort()) ===
        JSON.stringify(["pay_invoice", "request_manual_payment"]),
      "only the two required agent methods allowed (no empty list)",
    );
    check(BigInt(fc.allowance) === allowance, "limited 1 NEAR gas allowance");

    // A connection whose keystore holds ONLY the restricted key.
    const fakStore = new keyStores.InMemoryKeyStore();
    await fakStore.setKey(NETWORK_ID, IDS.agent, fak);
    const fakNear = new Near({ networkId: NETWORK_ID, nodeUrl: h.rpcUrl, keyStore: fakStore });
    const restrictedAgent = await fakNear.account(IDS.agent);

    // Allowed method #1: request_manual_payment. (Use the returned id rather
    // than a queue-length delta: creating a request lazily expires older ones.)
    const idsBefore = await nextPaymentId();
    await change(restrictedAgent, h.contractId, "request_manual_payment", {
      vendor_id: IDS.vendor,
      amount: ONE.toString(),
      memo: "via restricted key",
    });
    const newId = (await nextPaymentId()) - 1n;
    check(newId === idsBefore, "allowed method created a request");
    const pendingAfter = await view<any[]>(h, "get_pending_requests");
    check(pendingAfter.some((p) => Number(p.id) === Number(newId)), "new request is pending");

    // Allowed method #2: pay_invoice (real signed invoice, real payment).
    const nonce = await vendorNonce();
    const inv = await mainInvoice({ nonce, amount: ONE });
    await change(restrictedAgent, h.contractId, "pay_invoice", signedArgs(inv, h.vendorKey));
    expectedLedger -= ONE;
    expectedSpent += ONE;
    await assertAccounting("after restricted-key payment");

    // Disallowed / admin / unrelated methods must be rejected by the chain.
    const configBefore = await config();
    const contractBalanceBefore = await balanceYocto(h.accounts.contract);
    const attackerBalanceBefore = await balanceYocto(h.accounts.attacker);

    await expectReject("FAK: admin method", () =>
      change(restrictedAgent, h.contractId, "set_max_payment", {
        max_payment: (999n * NEAR).toString(),
      }),
    );
    await expectReject("FAK: pause", () =>
      change(restrictedAgent, h.contractId, "pause", {}),
    );
    await expectReject("FAK: unrelated method", () =>
      change(restrictedAgent, h.contractId, "get_config", {}),
    );
    await expectReject("FAK: fund", () =>
      change(restrictedAgent, h.contractId, "fund", {}, 0n),
    );
    await expectReject("FAK: direct transfer", () =>
      restrictedAgent.sendMoney(IDS.attacker, ONE),
    );
    await expectReject("FAK: add key", () =>
      restrictedAgent.addKey(nearKeyPairFromSeed(new Uint8Array(32).fill(9)).getPublicKey()),
    );
    await expectReject("FAK: delete key", () =>
      restrictedAgent.deleteKey(fak.getPublicKey()),
    );
    await expectReject("FAK: deploy code", () =>
      restrictedAgent.deployContract(new Uint8Array([0, 97, 115, 109])),
    );

    const configAfter = await config();
    check(configAfter.max_payment === configBefore.max_payment, "admin policy unchanged");
    check(configAfter.paused === configBefore.paused, "pause state unchanged");
    check((await balanceYocto(h.accounts.contract)) === contractBalanceBefore, "treasury untouched");
    check((await balanceYocto(h.accounts.attacker)) === attackerBalanceBefore, "attacker received nothing");

    // The restricted key is still usable after the rejections.
    const idsBefore2 = await nextPaymentId();
    await change(restrictedAgent, h.contractId, "request_manual_payment", {
      vendor_id: IDS.vendor,
      amount: ONE.toString(),
      memo: "still works",
    });
    check((await nextPaymentId()) - 1n === idsBefore2, "allowed method still works");
  });

  // ---- Final invariants -------------------------------------------------
  await test("T26 final ledger equals treasury funding minus successful payments", async () => {
    await assertAccounting("final");
    const t = await treasury();
    const expectedAvailable = expectedLedger - 2n * NEAR - 1n * NEAR;
    check(BigInt(t.available) === expectedAvailable, `available ${t.available} != ${expectedAvailable}`);
    const recent = await view<any[]>(h, "get_recent_payments", { limit: 100 });
    check(Array.isArray(recent) && recent.length > 0, "recent payments view returns records");
  });
} finally {
  await h.sandbox.tearDown();
}

// ---------------------------------------------------------------------------
// Summary
// ---------------------------------------------------------------------------

const failed = results.filter((r) => !r.ok);
console.log(`\n${results.length - failed.length}/${results.length} sandbox checks passed`);
if (failed.length > 0) {
  console.error("FAILED:");
  for (const r of failed) console.error(`  - ${r.name}`);
  process.exitCode = 1;
}
