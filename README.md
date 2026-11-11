# InvoiceGuard

On-chain payment authorization layer for AI agents on NEAR. The agent can
*request* payments; the contract independently decides whether a payment is
authorized.

> **Testnet prototype. Not audited. Not production-ready.** A valid vendor
> signature proves authorization by the registered key, not delivery of goods.
> Contract security does not protect against a malicious full-access account
> owner. NEAR transfers are asynchronous; callback behavior matters.

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

## Trust assumptions & security invariants

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
- No withdrawal, no self-upgrade, no admin method that edits stored payment
  fields. There is no way for the agent to change code, keys, or limits.

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
- The oldest non-terminal payment can temporarily block history trimming.
- Daily `spent` totals for pruned past days are dropped (per-payment records
  keep the accounting day).
- A failed transfer keeps the ledger intact but the nonce stays consumed.
- The administrator is a single account (no multisig / role hierarchy).
- The operations headroom (1 NEAR) is a fixed constant, not configurable.

## Checkpoint status

- **CP0** environment verification: completed and approved.
- **CP1** contract implementation: implemented; 90 unit tests, 9 vector tests,
  26 Sandbox checks, attack demo all executed successfully.
- **CP2** Testnet deployment: **not performed** — requires explicit approval.
- **CP3/CP4** documentation/demo polish and submission: pending.
