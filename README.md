# InvoiceGuard

**A compromised agent should not become a compromised treasury.**

InvoiceGuard is an on-chain payment authorization layer for AI agents on NEAR.
The agent may *request* a payment; a smart contract independently decides
whether that payment is authorized. Even a fully manipulated agent cannot make
the treasury pay outside the contract's rules.

> **This project is demonstrated in a local NEAR Sandbox only — not in
> Testnet and not in Mainnet.** CP2 (Testnet deployment) was deliberately
> skipped. No real accounts, wallets, credentials or public-network
> transactions are used anywhere in this repository.
>
> **Testnet prototype. Not audited. Not production-ready.** A valid vendor
> signature proves authorization by the registered key, not delivery of goods
> or service quality. Contract security does not protect against a malicious
> full-access account owner. NEAR transfers are asynchronous; callback
> behaviour matters.

Submission and presentation materials: [`docs/SUBMISSION.md`](docs/SUBMISSION.md),
[`docs/VIDEO_SCRIPT.md`](docs/VIDEO_SCRIPT.md).

---

## 1. Problem

AI agents can execute payments, but they can also be manipulated through
malicious instructions, compromised workflows or prompt injection. A plain
agent wallet has permission to transfer funds without independently verifying
*why* a payment is made, *who* authorized it and *where* the money goes.

## 2. Solution

The authority to pay is moved out of the agent and into a NEAR smart contract.
Two strictly separate paths exist:

1. **Vendor-signed invoices.** A registered vendor signs a canonical Borsh
   payload. The contract verifies the Ed25519 signature, domain separation
   (protocol / version / network / contract), the registered payout account,
   expiration, a per-vendor monotonic nonce, per-payment and daily limits, and
   treasury reserves — before scheduling a transfer.
2. **Manual payment requests.** The agent stores a request; it stays
   `pending_approval` until an administrator approves **the exact stored
   request**, by request id only. This path is never represented as a signed
   invoice.

The agent never supplies a public key, a recipient, an admin decision or a
limit change. It only submits invoices and requests.

## 3. How the contract decides

For `pay_invoice(invoice, signature)` the checks run, in order, **before any
state mutation**: caller is the configured agent → not paused → domain fields
match → amount > 0 → vendor exists and is active → recipient equals the
registered payout account → nonce equals the vendor's expected nonce → invoice
not expired → Ed25519 signature over the canonical payload verifies against
the *registered* key → per-payment max → daily limit including existing
reservations → available internal funds and physical unlocked balance.

Only then: the nonce is consumed, a `PROCESSING` payment record is stored, the
amount is reserved against its UTC day, and a `Promise::transfer` plus a
`#[private]` callback is scheduled. The callback finalizes exactly once:
`PAID` (ledger debited, daily `spent` increased) or `FAILED` (ledger
unchanged, reservation released). A failed transfer keeps the nonce consumed.

`available = ledger - total_reserved - reserve_floor - 1 NEAR headroom`.

## 4. Architecture

| Component | Role |
|---|---|
| `contract/src/config.rs` | policy constants, TTLs, gas, storage keys |
| `contract/src/types.rs` | persisted records, JSON views, `StorageKey` |
| `contract/src/invoice.rs` | canonical Borsh payload + domain validation |
| `contract/src/admin.rs` | admin policy + vendor registry |
| `contract/src/treasury.rs` | ledger, reservations, daily accounting, funding |
| `contract/src/payments.rs` | invoice/manual paths + private callback |
| `contract/src/views.rs` | read-only views |
| `contract/src/events.rs` | NEP-297 events |
| `contract/src/tests/` | 90 Rust unit tests |
| `ts/src/borsh.ts` | TypeScript canonical Borsh (`bigint`, never `Number`) |
| `ts/src/keys.ts` | Ed25519 vendor keys / signatures |
| `ts/src/sandbox.ts` | Sandbox harness: start node, create accounts, deploy, init |
| `scripts/demo.ts` | final CLI demonstration (8 scenarios) |
| `scripts/sandbox-test.ts` | 26 Sandbox integration checks |
| `scripts/gen-vectors.ts` | shared Rust↔TS test-vector generator |
| `tests/vectors.json` | shared canonical-bytes / signature vectors |

The AI agent is represented by a deterministic mock client with a restricted
NEAR function-call access key. No backend, database, LLM, dashboard or
external service is involved.

## 5. Exact toolchain

Verified in this repository (do not substitute without a concrete reason):

| Dependency | Version |
|---|---|
| Rust / cargo | 1.99.1 (stable) |
| `wasm32-unknown-unknown` target | installed via rustup |
| `near-sdk` | `=5.30.1` (`legacy`; dev adds `unit-testing`) |
| `ed25519-dalek` (dev only) | `=2.2.0` |
| Node.js | 24.14.1 |
| npm | 11.17.0 |
| `near-sandbox` | `0.3.5` (downloads `neard` **2.14.0**) |
| `near-api-js` | `5.1.0` |
| `bs58` | `6.0.0` |
| `tweetnacl` | `1.0.3` |
| `tsx` | `4.20.6` |
| `typescript` | `5.9.3` |
| `@types/node` | `24.10.1` |
| `near-cli` | `4.0.13` — verified in CP0 only |

`near-cli` is **not** used by the local demo: deployment and calls are made
programmatically with `near-api-js` + `near-sandbox`. No extra CLI install is
needed.

`near-sdk` 5.30.1 requires `--cfg near` at WASM build time for on-chain host
functions. Without it `env::panic_str` falls back to a local `panic!()` and the
runtime sees an opaque `unreachable` trap instead of the error code. Use
`npm run build:wasm` (it sets `RUSTFLAGS="--cfg near"`). Do **not** set that
flag globally: native `cargo test` uses the SDK's unit-testing host functions.

## 6. Reproduce from scratch

### 6.1 Install dependencies

```bash
# Rust target (only needed once on a fresh machine)
rustup target add wasm32-unknown-unknown

# JavaScript dependencies (uses package-lock.json)
npm install
```

### 6.2 Build the WASM contract

```bash
npm run build:wasm
# -> target/wasm32-unknown-unknown/release/invoiceguard.wasm
```

This runs `RUSTFLAGS="--cfg near" cargo build --target wasm32-unknown-unknown --release`.

### 6.3 Start a local Sandbox and deploy the contract

There is no separate daemon to start. `startHarness()` in
`ts/src/sandbox.ts` does all of it programmatically:

1. `Sandbox.start({})` from `near-sandbox` 0.3.5 launches an isolated
   `neard` 2.14.0 node (the binary is downloaded once on first run; network
   access is required for that first download).
2. Creates the accounts `admin.sandbox`, `agent.sandbox`, `vendor.sandbox`,
   `vendor2.sandbox`, `payout.sandbox`, `payout2.sandbox`,
   `attacker.sandbox` and the contract account `guard.sandbox`.
3. Deploys the compiled WASM to `guard.sandbox` and calls
   `new(admin, agent, "sandbox", 5 NEAR, 20 NEAR, 2 NEAR)`.
4. Returns typed `Account`/view/change helpers; the node is torn down at the
   end of the run.

Both commands below perform steps 1–4:

```bash
npm run demo           # deploy + 8-scenario demonstration
npm run test:sandbox   # deploy + 26 integration checks
```

`ghost.sandbox` is a valid named account that is intentionally **never
created**; it is used only to force a real failed transfer.

### 6.4 Run the demonstration

```bash
npm run demo
```

It prints real transaction outcomes, real contract error codes, real balance
deltas and real NEP-297 event logs. See [§8](#8-demonstration-scenarios).

### 6.5 All available commands

| Command | What it does |
|---|---|
| `npm run build:wasm` | build WASM with `--cfg near` |
| `npm run test:unit` | `cargo test` — 90 Rust unit tests |
| `npm run vectors` | regenerate `tests/vectors.json` |
| `npm run test:vectors` | 9 TypeScript shared-vector tests |
| `npm run test:sandbox` | 26 Sandbox integration checks |
| `npm run demo` | final 8-scenario CLI demonstration |
| `npm run typecheck` | `tsc --noEmit` |

No credentials, key files or `.env` files are needed. All test keys are
deterministic and held in memory; the only external key material is the
well-known public default Sandbox key shipped inside the `near-sandbox`
package.

## 7. Verified results (CP1, already executed)

| Suite | Result |
|---|---|
| `cargo test` | 90 passed, 0 failed |
| `npm run test:vectors` | 9 passed, 0 failed |
| `npm run test:sandbox` | 26/26 checks passed |
| `npm run demo` | all 8 scenarios, assertions passed |
| `npm run typecheck` | clean |
| `npm run build:wasm` | WASM built, 28 exports |

These results are from CP1; no additional stress-test cycles were run for the
submission. The Sandbox suite covers: real recipient balance change, forged /
tampered / redirected / replayed / expired / cross-domain invoices, limits,
failed-transfer accounting, direct private-callback invocation, manual
approve/reject/expiry, and restricted function-call key permissions.

## 8. Demonstration scenarios

`npm run demo` executes, in order, against a freshly deployed local Sandbox:

1. Valid signed invoice → `paid`, payout balance increases by exactly 1 NEAR.
2. Amount tampering (signature over 1 NEAR, submitted as 2) → `E_BAD_SIGNATURE`.
3. Recipient substitution (correctly signed for `attacker.sandbox`) → `E_RECIPIENT_MISMATCH`.
4. Replay of the consumed invoice → `E_NONCE_MISMATCH`.
5. Expired invoice → `E_INVOICE_EXPIRED`.
6. Manual request → `pending_approval`, no funds moved → separate admin approval → `paid`.
7. Payment above the per-payment limit (6 > 5 NEAR) → `E_OVER_MAX_PAYMENT`.
8. Failed transfer to non-existent `ghost.sandbox` → `failed`, `payment_failed` event, ledger unchanged, `total_reserved = 0`, nonce stays consumed.

Final state: only the two authorized 1 NEAR payments left the ledger; the
attacker balance is unchanged.

## 9. Canonical invoice payload

Borsh, fixed field order (must match `ts/src/borsh.ts` exactly):

| # | field | type |
|---|---|---|
| 1 | `protocol_id` | String (`"invoiceguard"`) |
| 2 | `protocol_version` | u32 (`1`) |
| 3 | `network_id` | String |
| 4 | `contract_id` | AccountId |
| 5 | `vendor_id` | AccountId |
| 6 | `nonce` | u64 (starts at 1) |
| 7 | `recipient` | AccountId |
| 8 | `amount` | u128 yoctoNEAR |
| 9 | `expiration_unix_sec` | u64, Unix seconds (UTC), exclusive |

AccountId has the Borsh layout of String (u32 LE length + UTF-8). All
integers are little-endian. `tests/vectors.json` is shared between Rust and
TypeScript; both sides assert byte equality and signature validity.

## 10. Contract API

- Init: `new(admin, agent, network_id, max_payment, daily_limit, reserve_floor)`
- Funding: `fund()` (payable, anyone)
- Admin: `register_vendor`, `set_vendor_payout`, `rotate_vendor_key`,
  `set_vendor_active`, `set_agent`, `set_max_payment`, `set_daily_limit`,
  `set_reserve_floor`, `set_request_ttl_seconds`, `pause`, `unpause`,
  `approve_manual_payment`, `reject_manual_payment`
- Agent: `pay_invoice(invoice, signature)`, `request_manual_payment(vendor_id, amount, memo)`
- Private: `on_transfer_complete(payment_id)`
- Views: `get_config`, `get_invoice_domain`, `get_vendor`, `get_vendors`,
  `get_treasury`, `get_payment`, `get_recent_payments`, `get_pending_requests`

`amount` arguments are `U128` decimal strings; `nonce`, `request_id` and
`ttl_seconds` are JSON numbers. Signatures are base64 `Base64VecU8`.

## 11. Security invariants

- `env::predecessor_account_id()` is the authorization subject.
- The signing key and payout account come only from the administrator-managed
  registry, never from the agent.
- `available = ledger - total_reserved - reserve_floor - 1 NEAR headroom`
  (saturating). All financial math is `u128` with checked arithmetic.
- A nonce is consumed **before** the transfer is scheduled and stays consumed
  on failure; replay requires a newly signed invoice.
- The private callback finalizes a `PROCESSING` payment exactly once.
- Replay protection lives in vendor nonces, not in payment history (history is
  trimmed to 100 terminal records).
- No withdrawal method and **no contract-level self-upgrade method**; no admin
  method edits stored payment fields. The agent cannot change code, keys, or
  limits.

## 12. Trust assumptions

These are deliberate boundaries of the threat model, not defects:

1. **Full-access account owners are fully trusted.** The contract has no
   self-upgrade or withdrawal *method*, but a holder of a full-access key to
   the **contract account** can redeploy the code via a NEAR `DeployContract`
   action and drain the balance; a holder of a full-access key to the **admin
   account** can change limits, the vendor registry, pause state and approve
   manual requests. The restricted function-call key only limits the agent.
2. **The administrator controls the vendor registry.** A manual request stores
   `vendor_id`, `amount` and `memo`, but **not** a recipient: the recipient is
   resolved from the registry at approval time. Therefore
   `approve_manual_payment(request_id)` cannot be subverted through call
   parameters, but an admin can change the registered payout between request
   creation and approval.
3. **A vendor signature proves key authorization, not delivery or service
   quality.** Recipient binding to the registered payout account is the
   additional control against a compromised signing key.
4. **The 1 NEAR operations headroom is a simplification**, not a proof of
   sufficiency under any load.
5. **Asynchronous transfers and callbacks.** A payment is final only after the
   private callback runs; a callback that fails closed leaves the payment
   `PROCESSING` with its reservation held.

## 13. What InvoiceGuard does and does not guarantee

**Does:** constrain the agent's financial authority independently of its
behaviour; bind every payout to a registered destination; reject forged,
tampered, replayed, expired, cross-network/cross-contract and over-limit
invoices; keep an internal ledger with correct failed-transfer accounting;
require a separate human decision for manual payments.

**Does not:** prevent prompt injection or malicious instructions from reaching
the agent (it only makes them financially harmless); protect against a
malicious full-access account owner; prove delivery of goods; replace a
professional audit; guarantee production-grade storage/gas headroom.

## 14. Known limitations

- Prototype for local Sandbox / future Testnet only; no professional audit.
- **The 1 NEAR operations headroom is a fixed simplification**, not a
  guarantee of sufficient storage/gas headroom under any load. It is not
  computed from `env::storage_usage()` and is not configurable.
- **State bounds are conditional on reservations resolving.** `trim_old_days`
  stops at the first day with `reserved > 0` and `trim_payment_history` stops
  at the first non-terminal payment. A payment stuck in `PROCESSING` can block
  both trimming loops, so daily entries and payment records may accumulate.
  This is a liveness/storage-growth caveat, **not** a daily-limit bypass.
- Daily `spent` totals for pruned past days are dropped (per-payment records
  keep the accounting day). Views do not roll the UTC day.
- A failed transfer keeps the ledger intact but the nonce stays consumed.
- The administrator is a single account (no multisig / role hierarchy).

## 15. Checkpoint status

- **CP0** environment verification: completed and approved.
- **CP1** contract implementation: completed; all suites executed successfully.
- **Pre-CP2 security review**: read-only, 5 areas reviewed, no blockers.
- **CP2 Testnet deployment**: **skipped by decision** — never performed.
- **CP3** documentation: complete.
- **CP4** submission: materials prepared, **not submitted / not published**.
