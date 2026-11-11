/**
 * InvoiceGuard final CLI demonstration (deterministic mock agent).
 *
 * Every step is a real NEAR Sandbox transaction or view call. Nothing is
 * faked: the printed statuses, balances and error strings are read back from
 * the contract / chain. `assert` only checks those real results.
 *
 * Eight scenarios, in order:
 *   1. Valid signed invoice -> paid, real balance change.
 *   2. Amount tampering (same signature) -> rejected.
 *   3. Recipient substitution (even correctly signed) -> rejected.
 *   4. Replay of the paid invoice -> rejected.
 *   5. Expired invoice -> rejected.
 *   6. Manual request -> stays pending -> separate admin approval -> paid.
 *   7. Payment above the per-payment limit -> rejected.
 *   8. Failed transfer -> reservation rolled back, ledger unchanged.
 *
 * The script starts its own local Sandbox, deploys + initializes the WASM,
 * runs the scenarios, then tears the node down. No Testnet/Mainnet involved.
 *
 * Run with: npm run demo
 */

import assert from "node:assert/strict";

import {
  IDS,
  YOCTO_PER_NEAR,
  balanceYocto,
  blockTimestampSec,
  buildInvoice,
  change,
  defaultExpiry,
  signedArgs,
  startHarness,
  u64ToJson,
  view,
  type Harness,
} from "../ts/src/sandbox.ts";
import type { SignedInvoice } from "../ts/src/borsh.ts";

const NEAR = YOCTO_PER_NEAR;
const ONE = 1n * NEAR;

interface Attempt {
  ok: boolean;
  detail: string;
  outcome?: any;
}

/** Run a transaction; return the real outcome or the real error text. */
async function attempt(fn: () => Promise<unknown>): Promise<Attempt> {
  try {
    const outcome: any = await fn();
    return { ok: true, detail: JSON.stringify(outcome?.status ?? {}), outcome };
  } catch (error) {
    return { ok: false, detail: error instanceof Error ? error.message : String(error) };
  }
}

/** All contract logs from a final execution outcome. */
function contractLogs(outcome: any): string[] {
  const logs: string[] = [];
  const push = (o: any) => {
    if (Array.isArray(o?.outcome?.logs)) logs.push(...o.outcome.logs);
  };
  push(outcome?.transaction_outcome);
  for (const r of outcome?.receipts_outcome ?? []) push(r);
  return logs;
}

function heading(n: number, title: string): void {
  console.log(`\n=== ${n}. ${title} ===`);
}

/** Build a correctly domain-separated invoice for the local Sandbox. */
async function invoice(
  h: Harness,
  fields: {
    nonce: bigint;
    amount: bigint;
    recipient?: string;
    vendorId?: string;
    expiry?: bigint;
  },
): Promise<SignedInvoice> {
  return buildInvoice(h, {
    vendorId: fields.vendorId ?? IDS.vendor,
    nonce: fields.nonce,
    recipient: fields.recipient ?? IDS.payout,
    amount: fields.amount,
    expirationUnixSec: fields.expiry ?? (await defaultExpiry(h)),
  });
}

async function latestPaymentId(h: Harness): Promise<bigint> {
  return BigInt((await view<any>(h, "get_config")).next_payment_id) - 1n;
}

async function main(): Promise<void> {
  const h: Harness = await startHarness();
  const summary: string[] = [];

  try {
    // ---- Setup: registry + treasury (all real transactions) -------------
    await change(h.accounts.admin, h.contractId, "register_vendor", {
      vendor_id: IDS.vendor,
      signing_key: h.vendorKey.nearPublicKey,
      payout_account: IDS.payout,
    });
    // Vendor 2 pays to a valid named account that is deliberately never
    // created: used only to force a real failed transfer in scenario 8.
    await change(h.accounts.admin, h.contractId, "register_vendor", {
      vendor_id: IDS.vendor2,
      signing_key: h.vendor2Key.nearPublicKey,
      payout_account: IDS.ghost,
    });
    await change(h.accounts.admin, h.contractId, "fund", {}, 100n * NEAR);

    const start = await view<any>(h, "get_treasury");
    const payoutBefore = await balanceYocto(h.accounts.payout);
    const attackerBefore = await balanceYocto(h.accounts.attacker);

    console.log("InvoiceGuard — local NEAR Sandbox demo (real transactions)");
    console.log("NOT a Testnet/Mainnet deployment. No real accounts or wallets involved.");
    console.log(`contract:       ${h.contractId}`);
    console.log(`agent:          ${IDS.agent}`);
    console.log(`admin:          ${IDS.admin}`);
    console.log(`vendor payout:  ${IDS.payout}`);
    console.log(`attacker:       ${IDS.attacker}`);
    console.log(`ledger:         ${start.ledger_balance} yoctoNEAR`);
    console.log(`limits:         max_payment=5 NEAR, daily_limit=20 NEAR`);

    // ------------------------------------------------------------------
    heading(1, "Valid signed invoice: 1 NEAR to the registered payout account");
    const legit = await invoice(h, { nonce: 1n, amount: ONE });
    const r1 = await attempt(() =>
      change(h.accounts.agent, h.contractId, "pay_invoice", signedArgs(legit, h.vendorKey)),
    );
    console.log(`pay_invoice -> ${r1.ok ? "ACCEPTED" : "REJECTED"}  ${r1.detail}`);
    const id1 = await latestPaymentId(h);
    const p1 = await view<any>(h, "get_payment", { payment_id: u64ToJson(id1) });
    console.log(`payment #${id1}: status=${p1.status} recipient=${p1.recipient}`);
    console.log(`payout balance delta: ${(await balanceYocto(h.accounts.payout)) - payoutBefore} yoctoNEAR`);
    assert.ok(r1.ok, "valid invoice must be accepted");
    assert.equal(p1.status, "paid");
    summary.push(`1 valid invoice           -> ACCEPTED, payment #${id1} = ${p1.status}`);

    // ------------------------------------------------------------------
    heading(2, "Amount tampering: signature over 1 NEAR, submitted as 2 NEAR");
    const signedOne = await invoice(h, { nonce: 2n, amount: ONE });
    const tampered = signedArgs(signedOne, h.vendorKey);
    tampered.invoice.amount = (2n * NEAR).toString();
    const r2 = await attempt(() =>
      change(h.accounts.agent, h.contractId, "pay_invoice", tampered),
    );
    console.log(`pay_invoice -> ${r2.ok ? "ACCEPTED (BAD)" : "REJECTED"}`);
    console.log(`contract response: ${r2.detail}`);
    assert.equal(r2.ok, false, "tampered amount must be rejected");
    assert.ok(r2.detail.includes("E_BAD_SIGNATURE"), "real contract error code");
    summary.push(`2 amount tampering        -> REJECTED (E_BAD_SIGNATURE)`);

    // ------------------------------------------------------------------
    heading(3, "Recipient substitution: correctly signed for attacker.sandbox");
    const redirected = await invoice(h, {
      nonce: 2n,
      amount: ONE,
      recipient: IDS.attacker,
    });
    const r3 = await attempt(() =>
      change(h.accounts.agent, h.contractId, "pay_invoice", signedArgs(redirected, h.vendorKey)),
    );
    console.log(`pay_invoice -> ${r3.ok ? "ACCEPTED (BAD)" : "REJECTED"}`);
    console.log(`contract response: ${r3.detail}`);
    assert.equal(r3.ok, false, "recipient substitution must be rejected");
    assert.ok(r3.detail.includes("E_RECIPIENT_MISMATCH"), "real contract error code");
    summary.push(`3 recipient substitution  -> REJECTED (E_RECIPIENT_MISMATCH)`);

    // ------------------------------------------------------------------
    heading(4, "Replay of the already-consumed invoice (nonce 1)");
    const r4 = await attempt(() =>
      change(h.accounts.agent, h.contractId, "pay_invoice", signedArgs(legit, h.vendorKey)),
    );
    console.log(`pay_invoice -> ${r4.ok ? "ACCEPTED (BAD)" : "REJECTED"}`);
    console.log(`contract response: ${r4.detail}`);
    assert.equal(r4.ok, false, "replay must be rejected");
    assert.ok(r4.detail.includes("E_NONCE_MISMATCH"), "real contract error code");
    summary.push(`4 invoice replay          -> REJECTED (E_NONCE_MISMATCH)`);

    // ------------------------------------------------------------------
    heading(5, "Expired invoice");
    const expired = await invoice(h, {
      nonce: 2n,
      amount: ONE,
      expiry: (await blockTimestampSec(h.rpcUrl)) - 60n,
    });
    const r5 = await attempt(() =>
      change(h.accounts.agent, h.contractId, "pay_invoice", signedArgs(expired, h.vendorKey)),
    );
    console.log(`pay_invoice -> ${r5.ok ? "ACCEPTED (BAD)" : "REJECTED"}`);
    console.log(`contract response: ${r5.detail}`);
    assert.equal(r5.ok, false, "expired invoice must be rejected");
    assert.ok(r5.detail.includes("E_INVOICE_EXPIRED"), "real contract error code");
    summary.push(`5 expired invoice         -> REJECTED (E_INVOICE_EXPIRED)`);

    // ------------------------------------------------------------------
    heading(6, "Manual request + separate administrator approval");
    const created = await attempt(() =>
      change(h.accounts.agent, h.contractId, "request_manual_payment", {
        vendor_id: IDS.vendor,
        amount: ONE.toString(),
        memo: "manual request, needs admin approval",
      }),
    );
    const requestId = await latestPaymentId(h);
    const pending = await view<any>(h, "get_payment", { payment_id: u64ToJson(requestId) });
    const payoutBeforeApproval = await balanceYocto(h.accounts.payout);
    console.log(`request_manual_payment -> ${created.ok ? "CREATED" : created.detail}`);
    console.log(`request #${requestId}: status=${pending.status} recipient=${pending.recipient} amount=${pending.amount}`);
    console.log("no funds moved: request waits for an explicit administrator decision");

    const approved = await attempt(() =>
      change(h.accounts.admin, h.contractId, "approve_manual_payment", {
        request_id: u64ToJson(requestId),
      }),
    );
    const approvedPayment = await view<any>(h, "get_payment", { payment_id: u64ToJson(requestId) });
    console.log(`admin approve_manual_payment -> ${approved.ok ? "APPROVED" : "REJECTED"}  ${approved.detail}`);
    console.log(`status=${approvedPayment.status} recipient=${approvedPayment.recipient} amount=${approvedPayment.amount}`);
    console.log(`payout balance delta on approval: ${(await balanceYocto(h.accounts.payout)) - payoutBeforeApproval} yoctoNEAR`);
    assert.ok(created.ok && approved.ok, "manual flow must succeed");
    assert.equal(pending.status, "pending_approval");
    assert.equal(approvedPayment.status, "paid");
    assert.equal(approvedPayment.recipient, IDS.payout);
    assert.equal((await balanceYocto(h.accounts.payout)) - payoutBeforeApproval, ONE);
    summary.push(`6 manual + admin approval -> PENDING -> ${approvedPayment.status}, 1 NEAR`);

    // ------------------------------------------------------------------
    heading(7, "Above the per-payment limit (6 NEAR > 5 NEAR max)");
    const overLimit = await invoice(h, { nonce: 2n, amount: 6n * NEAR });
    const r7 = await attempt(() =>
      change(h.accounts.agent, h.contractId, "pay_invoice", signedArgs(overLimit, h.vendorKey)),
    );
    console.log(`pay_invoice -> ${r7.ok ? "ACCEPTED (BAD)" : "REJECTED"}`);
    console.log(`contract response: ${r7.detail}`);
    assert.equal(r7.ok, false, "over-limit payment must be rejected");
    assert.ok(r7.detail.includes("E_OVER_MAX_PAYMENT"), "real contract error code");
    summary.push(`7 over limit              -> REJECTED (E_OVER_MAX_PAYMENT)`);

    // ------------------------------------------------------------------
    heading(8, "Failed transfer to a non-existent named account: reservation rollback");
    const before8 = await view<any>(h, "get_treasury");
    const failing = await invoice(h, {
      vendorId: IDS.vendor2,
      nonce: 1n,
      recipient: IDS.ghost,
      amount: ONE,
    });
    const r8 = await attempt(() =>
      change(h.accounts.agent, h.contractId, "pay_invoice", signedArgs(failing, h.vendor2Key)),
    );
    const id8 = await latestPaymentId(h);
    const p8 = await view<any>(h, "get_payment", { payment_id: u64ToJson(id8) });
    const after8 = await view<any>(h, "get_treasury");
    const vendor2 = await view<any>(h, "get_vendor", { vendor_id: IDS.vendor2 });
    const failedEvent = contractLogs(r8.outcome).some((l) => l.includes("payment_failed"));
    console.log(`pay_invoice -> ${r8.ok ? "transfer scheduled" : "call rejected"}  ${r8.detail}`);
    console.log(`payment #${id8}: status=${p8.status} (transfer to ${IDS.ghost} does not exist)`);
    console.log(`payment_failed event emitted: ${failedEvent}`);
    console.log(`ledger before / after: ${before8.ledger_balance} / ${after8.ledger_balance}`);
    console.log(`total_reserved before / after: ${before8.total_reserved} / ${after8.total_reserved}`);
    console.log(`today_reserved before / after: ${before8.today_reserved} / ${after8.today_reserved}`);
    console.log(`vendor2 next_nonce (nonce stays consumed): ${vendor2.next_nonce}`);
    assert.ok(r8.ok, "outer call succeeds; the transfer receipt itself fails");
    assert.equal(p8.status, "failed");
    assert.ok(failedEvent, "real payment_failed NEP-297 event");
    assert.equal(BigInt(after8.ledger_balance), BigInt(before8.ledger_balance));
    assert.equal(BigInt(after8.total_reserved), 0n);
    assert.equal(BigInt(after8.today_reserved), 0n);
    assert.equal(BigInt(vendor2.next_nonce), 2n);
    summary.push(`8 failed transfer         -> status=failed, ledger unchanged, reserved=0`);

    // ------------------------------------------------------------------
    heading(9, "Final state read back from the contract");
    const final = await view<any>(h, "get_treasury");
    const v1 = await view<any>(h, "get_vendor", { vendor_id: IDS.vendor });
    console.log(`ledger_balance: ${final.ledger_balance}`);
    console.log(`total_reserved: ${final.total_reserved}`);
    console.log(`today_spent:    ${final.today_spent}`);
    console.log(`vendor next_nonce: ${v1.next_nonce}`);
    console.log(`payout balance delta: ${(await balanceYocto(h.accounts.payout)) - payoutBefore}`);
    console.log(`attacker balance unchanged: ${(await balanceYocto(h.accounts.attacker)) === attackerBefore}`);

    assert.equal(BigInt(final.ledger_balance), 98n * NEAR, "only the two legitimate 1 NEAR payments left the ledger");
    assert.equal(BigInt(final.today_spent), 2n * NEAR);
    assert.equal(BigInt(final.total_reserved), 0n);
    assert.equal(BigInt(v1.next_nonce), 2n);
    assert.equal((await balanceYocto(h.accounts.payout)) - payoutBefore, 2n * NEAR);
    assert.equal((await balanceYocto(h.accounts.attacker)), attackerBefore);

    console.log("\n--- Scenario summary (from real contract responses) ---");
    for (const s of summary) console.log(`  ${s}`);
    console.log("\nDemo assertions passed: only authorized payments changed balances.");
  } finally {
    await h.sandbox.tearDown();
  }
}

await main();
