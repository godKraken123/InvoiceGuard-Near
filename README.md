# InvoiceGuard

On-chain payment authorization layer for AI agents on NEAR. The agent can
*request* payments; the contract independently decides whether a payment is
authorized.

> **Testnet prototype. Not audited. Not production-ready.** A valid vendor
> signature proves authorization by the registered key, not delivery of goods.
> Contract security does not protect against a malicious full-access account
> owner. NEAR transfers are asynchronous; callback behavior matters.

See [Trust assumptions](#trust-assumptions) for what is explicitly **not**
protected: in particular, whoever holds a full-access key to the **contract
account** can redeploy its code at the NEAR account level, and whoever holds a
full-access key to the **admin account** can call every admin method. The
agent's restricted function-call key only constrains the agent.

## Problem / solution

An agent wallet can be manipulated (prompt injection, compromised workflow)
into paying an attacker. InvoiceGuard moves the decision to a smart contract:

1. **Vendor-signed invoices** — a registered vendor signs a canonical Borsh
   payload. The contract checks the signature, domain, payout binding,
   expiration, nonce, limits, and treasury before transferring.
2. **Manual requests** — the agent stores a request; an administrator approves
   the exact stored fields by request id only. This path is never represented
   as a signed invoice.

## Architecture

| Module | Responsibility |
|---|---|
| `contract/src/config.rs` | limits, TTLs, gas, storage keys |
| `contract/src/types.rs` | state records, JSON views, `StorageKey` |
| `contract/src/invoice.rs` | canonical Borsh payload + domain validation |
| `contract/src/admin.rs` | admin policy + vendor registry |
| `contract/src/treasury.rs` | ledger, reservations, daily accounting |
| `contract/src/payments.rs` | invoice/manual paths + private callback |
| `contract/src/views.rs` | read-only views |
| `contract/src/events.rs` | NEP-297 events |
| `ts/src/borsh.ts` | TypeScript canonical Borsh (`bigint`, never `Number`) |
| `ts/src/keys.ts` | Ed25519 vendor keys/signatures |
| `ts/src/sandbox.ts` | Sandbox harness |

## Security invariants

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

## Trust assumptions

These are deliberate boundaries of the threat model, not defects:

1. **Full-access account owners are fully trusted.** The contract has no
   self-upgrade or withdrawal *method*, but a holder of a full-access key to
   the **contract account** can redeploy the code via a NEAR `DeployContract`
   action and drain the balance; a holder of a full-access key to the **admin
   account** can change limits, the vendor registry, pause state, and approve
   manual requests. The restricted function-call key only limits the agent.
   Do not remove or weaken these recovery keys without explicit approval.
2. **The administrator controls the vendor registry.** `register_vendor`,
   `set_vendor_payout` and `rotate_vendor_key` are admin-only. A manual
   request stores `vendor_id`, `amount` and `memo`, but **not** a recipient:
   the recipient is resolved from the registry at approval time. Therefore
   `approve_manual_payment(request_id)` cannot be subverted through call
   parameters, but an admin can change the registered payout between request
   creation and approval. Both behaviours are intentional (§8.2 of the brief).
3. **A registered vendor signature proves key authorization, not delivery or
   service quality.** Recipient binding to the registered payout account is the
   additional control against a compromised signing key.
4. **The 1 NEAR operations headroom is a simplification**, not a proof of
   sufficiency under any load (see [Known limitations](#known-limitations)).
5. **Asynchronous transfers and callbacks.** A payment is final only after the
   private callback runs; a callback that fails closed leaves the payment
   `PROCESSING` with its reservation held.

## Canonical invoice payload

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

AccountId has the Borsh layout of String (u32 LE length + UTF-8). Strings and
integers are little-endian. `tests/vectors.json` is shared between Rust and
TypeScript; both sides assert byte equality and signature validity.

## Toolchain (pinned)

Rust 1.99.1, `near-sdk` 5.30.1, `near-sandbox` 0.3.5 (`neard` 2.14.0),
`near-api-js` 5.1.0, `near-cli` 4.0.13, Node 24, TypeScript 5.9.3.

`near-sdk` 5.30.1 requires `--cfg near` at WASM build time for on-chain host
functions. Without it `env::panic_str` falls back to a local `panic!()` and the
runtime sees an opaque `unreachable` trap instead of the error code. Use
`npm run build:wasm` (which sets `RUSTFLAGS="--cfg near"`). Do not set this flag
globally: native `cargo test` uses the SDK's unit-testing host functions.

## Reproduce

```bash
npm install
npm run build:wasm          # -> target/wasm32-unknown-unknown/release/invoiceguard.wasm
npm run test:unit           # 90 Rust unit tests
npm run vectors             # regenerate tests/vectors.json
npm run test:vectors        # 9 TypeScript vector tests
npm run test:sandbox        # 26 Sandbox integration checks (downloads neard 2.14.0 once)
npm run demo                # attack demonstration (real transactions)
npm run typecheck           # tsc --noEmit
```

Sandbox tests create accounts under `sandbox` (`admin.sandbox`,
`agent.sandbox`, `vendor.sandbox`, `payout.sandbox`, `attacker.sandbox`,
`guard.sandbox`) and tear the node down at the end. No credentials or keys are
written to disk; all keys are deterministic and in-memory.

## Demo scenario

`npm run demo` executes: a legitimate 1 NEAR invoice (paid, real balance
change), a manipulated attempt to send 500 NEAR to `attacker.sandbox`
(rejected), a replay (rejected), and an unauthorized manual request that stays
pending until the admin rejects it. All printed outcomes come from the chain.

## Contract API

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

## Known limitations

- Prototype on Testnet/Sandbox only; no professional audit.
- **The 1 NEAR operations headroom is a fixed simplification**, not a
  guarantee of sufficient storage/gas headroom under any load. It is not
  computed from `env::storage_usage()` and is not configurable. At
  `10^19` yoctoNEAR per byte it covers roughly 100 KB of state; normal
  bounded state is well below that, but the bound is conditional (next item).
- **State bounds are conditional on reservations resolving.** `trim_old_days`
  stops at the first day with `reserved > 0` and `trim_payment_history` stops
  at the first non-terminal payment. A payment stuck in `PROCESSING` (e.g. the
  fail-closed callback branch) therefore blocks both trimming loops: daily
  entries and payment records can then accumulate without a bound. This is a
  liveness/storage-growth caveat, **not** a daily-limit bypass — the limit is
  always checked against the current day's own entry.
- Daily `spent` totals for pruned past days are dropped (per-payment records
  keep the accounting day). Views do not roll the UTC day; `today_*` refers to
  `current_day`, i.e. the day of the last treasury operation.
- A failed transfer keeps the ledger intact but the nonce stays consumed.
- The administrator is a single account (no multisig / role hierarchy).

## Checkpoint status

- **CP0** environment verification: completed and approved.
- **CP1** contract implementation: implemented; 90 unit tests, 9 vector tests,
  26 Sandbox checks, attack demo all executed successfully.
- **Pre-CP2 security review** (read-only, no deployment, no code changes):
  5 areas reviewed; no blockers found; three documentation clarifications
  applied in CP3.
- **CP2** Testnet deployment: **not performed** — requires explicit approval.
- **CP3** documentation: trust assumptions, threat-model boundaries and
  limitations documented; reproducible instructions in place.
- **CP4** final demo/submission: pending.
