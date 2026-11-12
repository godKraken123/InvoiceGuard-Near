# InvoiceGuard — submission materials

> **A compromised agent should not become a compromised treasury.**

All materials below describe a **local NEAR Sandbox demonstration**. The
project has **not** been deployed to Testnet or Mainnet, has not been audited,
and is a prototype — not a production system. Nothing here claims otherwise.

---

## 1. Short description (for the submission form)

InvoiceGuard is a NEAR smart contract that acts as a payment-authorization
layer for AI agents. The agent can submit requests, but the contract — not the
agent — decides whether funds may move. It verifies Ed25519-signed vendor
invoices over a canonical Borsh payload, binds every payout to a registered
destination, enforces replay protection, spending limits and an internal
treasury ledger, and requires a separate administrator decision for manual
payments. A deterministic mock agent demonstrates that even a fully
manipulated agent cannot redirect funds. Demonstrated in a local NEAR Sandbox
with 90 unit tests, 9 shared Rust↔TypeScript vectors and 26 integration checks.

One-liner: **an on-chain guardrail that limits an agent's financial authority
independently of the agent's behaviour.**

## 2. Problem / Solution

**Problem.** AI agents can sign and execute payments, but they can also be
manipulated — prompt injection, malicious instructions, compromised workflows.
A plain agent wallet transfers funds without independently verifying *why*,
*who authorized it* and *where the money goes*.

**Solution.** Move the decision on-chain. Two strictly separate paths:

1. **Vendor-signed invoice** — registered vendor signs a canonical payload;
   contract checks signature, domain (protocol / version / network / contract),
   registered payout account, expiration, per-vendor nonce, limits and
   treasury reserves before scheduling a transfer.
2. **Manual request** — agent stores an immutable request; it stays pending
   until an admin approves the exact stored request by request id only.

The agent never supplies a public key, a recipient, a limit change or an
approval. It only submits invoices and requests.

**Important nuance:** InvoiceGuard does **not** prevent prompt injection. It
makes injected instructions financially harmless by capping what the agent's
key can actually cause the contract to do.

## 3. Architecture overview

```
   Vendor (off-chain)                AI agent (restricted FAK)
   Ed25519 signs canonical      ┌─── pay_invoice / request_manual_payment
   Borsh payload                │
                                ▼
              ┌─────────────────────────────────────────┐
              │  InvoiceGuard contract (NEAR / Rust)    │
              │  ─ access control (agent / admin)       │
              │  ─ vendor registry (key + payout)       │
              │  ─ signature + domain + nonce checks    │
              │  ─ ledger, reservations, daily limits   │
              │  ─ Promise::transfer + private callback │
              │  ─ NEP-297 events, bounded history      │
              └─────────────────────────────────────────┘
                    │                        ▲
                    ▼ transfer               │ approve / reject / configure
              Registered payout        Administrator (human)
```

Rust modules: `config`, `types`, `invoice`, `admin`, `treasury`, `payments`,
`views`, `events`. TypeScript tooling: canonical Borsh (`bigint`), Ed25519
signing, Sandbox harness, shared vectors. No backend, no database, no LLM, no
dashboard, no external services. Native NEAR only.

## 4. Security model

**Authorization subject:** `env::predecessor_account_id()` — never a
user-supplied account id.

**Key rules**

- Registered vendor Ed25519 key is the only accepted signer; the key and the
  payout account come from the admin-managed registry, never from the agent.
- Domain separation: `protocol_id`, `protocol_version`, `network_id`,
  `contract_id` are all signed fields.
- Payout binding: `invoice.recipient == vendor.payout_account`, checked even
  when the signature is valid.
- Replay protection: per-vendor monotonic nonce (starts at 1), consumed
  **before** the transfer is scheduled and kept consumed on failure.
- Treasury: internal ledger, `available = ledger - reserved - reserve_floor -
  1 NEAR headroom`, checked arithmetic, per-payment + daily UTC limits,
  reservations counted against the daily limit.
- Asynchronous safety: `#[private]` callback finalizes a `PROCESSING` payment
  exactly once; failed transfer releases the reservation and leaves the ledger
  untouched.
- Agent's restricted function-call key: only `pay_invoice` and
  `request_manual_payment` on the contract, with a limited gas allowance.

**Precision:** `u128` in Rust, `bigint` / decimal strings in TypeScript,
never JavaScript `Number` for money.

## 5. Threat model

| # | Threat | Contract enforcement | Evidence | Residual trust / limitation |
|---|---|---|---|---|
| 1 | Prompt injection / malicious instructions | Agent cannot set recipient, amount policy, keys or approvals; contract re-verifies everything | `demo` 2–3, Sandbox T06–T08 | Does not stop injection reaching the agent; only caps its financial effect |
| 2 | Compromised agent | All authority checks use `predecessor_account_id()`; agent can only call two methods | Sandbox T17, T25 | A full-access key on the agent account is out of scope |
| 3 | Forged vendor signature | Ed25519 over canonical Borsh with the registered key | Sandbox T06, unit `forged_signature_is_rejected` | Vendor key must be kept safe |
| 4 | Compromised vendor signing key | Recipient must equal registered payout; admin can rotate/deactivate | Sandbox T08, T16; unit `key_rotation_invalidates_old_signatures` | Admin is trusted to react; signature ≠ delivery |
| 5 | Recipient substitution | `E_RECIPIENT_MISMATCH` before signature verification | Sandbox T08, `demo` 3 | — |
| 6 | Invoice replay | Monotonic nonce, consumed before transfer | Sandbox T09, T20; unit `replaying_the_same_invoice_is_rejected` | A failed transfer still consumes the nonce (chosen model) |
| 7 | Cross-network replay | `network_id` is a signed domain field | Sandbox T12 | — |
| 8 | Cross-contract replay | `contract_id` is a signed domain field | Sandbox T13 | — |
| 9 | Expired invoice | `expiration_unix_sec` exclusive deadline | Sandbox T11, `demo` 5 | Depends on block timestamp |
| 10 | Inactive / unknown vendor | Registry lookup + `active` flag | Sandbox T15, T16 | Admin manages the registry |
| 11 | Treasury overspending | Ledger, reserve floor, headroom, `available_funds()` | unit `ledger_must_cover_payment_over_floor_and_headroom` | Headroom is a fixed simplification |
| 12 | Concurrent payments | Reservations created after all checks; checked arithmetic | unit `reservations_count_against_the_daily_limit` | — |
| 13 | Daily-limit bypass via pending reservations | `spent + reserved + amount <= daily_limit` | Sandbox T19, unit `daily_limit_counts_spent_and_reserved` | Original UTC day stored on the payment |
| 14 | Failed asynchronous transfer | Callback releases reservation, ledger unchanged, `payment_failed` event | Sandbox T20, `demo` 8 | Nonce stays consumed |
| 15 | Callback replay / direct invocation | `#[private]` + `PROCESSING` state check + exactly one promise result | Sandbox T21, unit `double_finalization_is_rejected` | Oversized promise result fails closed, can leave `PROCESSING` |
| 16 | Manual approval parameter substitution | `approve_manual_payment(request_id)` uses only stored fields | Sandbox T22 | Recipient is resolved from the registry at approval time (admin-controlled) |
| 17 | Manual-request storage exhaustion | 32 pending cap, 24 h TTL, 256-byte memo, lazy expiry | unit `pending_queue_is_bounded`, Sandbox T24 | — |
| 18 | Unauthorized administration | Every config method requires `is_admin()` | Sandbox T03, unit `auth_tests` | Single admin account, no multisig |
| 19 | Malicious full-access account owner | **Not protected** — explicit trust assumption | README §12 | Contract-account key can redeploy code; admin key can reconfigure |
| 20 | Storage exhaustion / insufficient deposit | Bounded vendors, requests and history; 1 NEAR headroom | unit `history_retention_trims_terminal_payments_only` | Bounds are conditional on reservations resolving; headroom not computed from `storage_usage()` |

## 6. What InvoiceGuard protects — and what it does not

**Protects**
- The agent's financial authority, independently of its behaviour.
- Every payout destination (registered payout binding).
- Invoice integrity, freshness, domain, nonce and limits.
- Treasury correctness including failed transfers.
- Separation of signed-invoice and human-approved payment paths.

**Does not guarantee**
- That prompt injection or malicious instructions never reach the agent.
- Protection against a malicious full-access account owner.
- Delivery of goods, service quality, or vendor honesty beyond key ownership.
- Production readiness, a security audit, or sufficient storage/gas headroom
  under any load.
- Testnet/Mainnet behaviour — everything shown is a local Sandbox.

## 7. Demonstration scenarios

`npm run demo` (local Sandbox, real transactions):

1. Valid signed invoice → `paid`, payout +1 NEAR.
2. Amount tampering → `E_BAD_SIGNATURE`.
3. Recipient substitution → `E_RECIPIENT_MISMATCH`.
4. Invoice replay → `E_NONCE_MISMATCH`.
5. Expired invoice → `E_INVOICE_EXPIRED`.
6. Manual request → `pending_approval` → separate admin approval → `paid`.
7. Above per-payment limit → `E_OVER_MAX_PAYMENT`.
8. Failed transfer to non-existent `ghost.sandbox` → `failed`, ledger
   unchanged, `total_reserved = 0`, `payment_failed` event, nonce consumed.

Final state: only the two authorized 1 NEAR payments left the ledger; attacker
balance unchanged. See [`VIDEO_SCRIPT.md`](VIDEO_SCRIPT.md) for the 2–3 minute
walkthrough.

## 8. Limitations and known risks

- Local Sandbox prototype only; Testnet deployment deliberately skipped; no
  audit.
- Fixed 1 NEAR operations headroom is a simplification, not a guarantee.
- State bounds are conditional on reservations resolving: a payment stuck in
  `PROCESSING` can block both trimming loops (liveness/storage growth, not a
  limit bypass).
- Single administrator; no multisig, no timelock, no role hierarchy.
- Daily `spent` totals for pruned past days are dropped; views do not roll the
  UTC day.
- A failed transfer keeps the ledger intact but consumes the nonce.
- No withdrawal and no contract-level self-upgrade method — but a full-access
  key to the contract account can redeploy code at the NEAR account level.

## 9. X post draft (do not publish yet)

```
A compromised agent should not become a compromised treasury.

InvoiceGuard is a NEAR smart contract that decides whether an AI agent's
payment is authorized — not the agent. Vendor-signed invoices (Ed25519 over
canonical Borsh) and admin-approved manual requests stay strictly separate.

Local NEAR Sandbox demo, 8 real scenarios: forged, tampered, redirected,
replayed, expired and over-limit invoices all rejected; a failed transfer
rolls back the reservation; attacker balance unchanged.

Not Testnet. Not audited. This shows payment constraints holding even when the
agent is manipulated — not a claim that prompt injection is "solved".

90 unit tests · 9 shared Rust↔TS vectors · 26 Sandbox checks
[repo link] [demo video]
#NEARHackathon #NEARProtocol #SmartContracts #AISecurity
```

## 10. Submission checklist (for the human owner)

- [ ] Public repository URL (not published yet)
- [ ] 2–3 minute demo video (not recorded yet)
- [ ] Short description above + tagline
- [ ] Track: Security (AI Security & Defense), secondary: Agents
- [ ] `npm run build:wasm` + `npm run demo` reproduction confirmed
- [ ] Confirm no secrets / credentials before publishing
- [x] MIT LICENSE added; `license = "MIT"` in `contract/Cargo.toml` and `package.json`
- [ ] Explicit approval before publishing the repo, X post or submission
