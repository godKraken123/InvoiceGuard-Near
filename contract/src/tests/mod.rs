//! Unit tests for InvoiceGuard.
//!
//! These tests run the contract logic directly against a mocked
//! blockchain (`near-sdk`'s `unit-testing` feature), which also makes
//! `env::ed25519_verify` perform *real* Ed25519 verification through the
//! same host implementation the NEAR runtime uses.
//!
//! What these tests deliberately do NOT cover (the wasm-level wrappers
//! are `#[cfg(target_arch = "wasm32")]`, so native calls bypass them):
//! - the `#[private]` predecessor/deposit check on `on_transfer_complete`,
//! - the `#[init]` "already initialized" state check,
//! - JSON argument parsing.
//! Those are covered by the Sandbox integration suite in
//! `tests/sandbox/` against the compiled wasm.

mod auth_tests;
mod invoice_tests;
mod manual_tests;
mod payment_tests;
mod vendor_tests;

use ed25519_dalek::{Signer, SigningKey};
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, AccountId, CurveType, NearToken, PromiseResult, PublicKey, RuntimeFeesConfig, VMContext};

use crate::invoice::SignedInvoice;
use crate::Contract;

pub const DAY: u64 = 86_400;
pub const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Fixed test instant (2026-02-02T02:40:00Z). Day 20486.
pub const T0_SEC: u64 = 1_770_000_000;

/// yoctoNEAR for a whole number of NEAR.
pub fn yocto(near: u128) -> u128 {
    near * 10_u128.pow(24)
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

pub fn contract_acct() -> AccountId {
    "invoiceguard.unit.test.near".parse().unwrap()
}
pub fn admin_acct() -> AccountId {
    "admin.unit.test.near".parse().unwrap()
}
pub fn agent_acct() -> AccountId {
    "agent.unit.test.near".parse().unwrap()
}
pub fn vendor_acct() -> AccountId {
    "vendor.unit.test.near".parse().unwrap()
}
pub fn payout_acct() -> AccountId {
    "payout.unit.test.near".parse().unwrap()
}
pub fn vendor2_acct() -> AccountId {
    "vendor2.unit.test.near".parse().unwrap()
}
pub fn payout2_acct() -> AccountId {
    "payout2.unit.test.near".parse().unwrap()
}
pub fn attacker_acct() -> AccountId {
    "attacker.unit.test.near".parse().unwrap()
}

// ---------------------------------------------------------------------------
// Keys and signatures
// ---------------------------------------------------------------------------

/// Deterministic test keys (derived from fixed seeds, not `rand`).
pub fn vendor_key() -> SigningKey {
    SigningKey::from_bytes(&[1_u8; 32])
}
pub fn vendor2_key() -> SigningKey {
    SigningKey::from_bytes(&[2_u8; 32])
}
pub fn attacker_key() -> SigningKey {
    SigningKey::from_bytes(&[3_u8; 32])
}
pub fn rotated_key() -> SigningKey {
    SigningKey::from_bytes(&[4_u8; 32])
}

/// Wrap raw Ed25519 key bytes as a `near_sdk::PublicKey`.
pub fn pk_of(key: &SigningKey) -> PublicKey {
    PublicKey::from_parts(CurveType::ED25519, key.verifying_key().to_bytes().to_vec()).unwrap()
}

// ---------------------------------------------------------------------------
// Environments (contexts)
// ---------------------------------------------------------------------------

/// Base context: caller `caller`, contract account is `contract_acct()`,
/// timestamp `ts_sec` (seconds), and a generous physical balance of
/// 200 NEAR (all unlocked) for the defense-in-depth balance check.
pub fn ctx_builder(caller: &AccountId, ts_sec: u64) -> VMContextBuilder {
    let mut b = VMContextBuilder::new();
    b.current_account_id(contract_acct())
        .signer_account_id(caller.clone())
        .predecessor_account_id(caller.clone())
        .block_timestamp(ts_sec * NANOS_PER_SEC)
        .account_balance(NearToken::from_near(200));
    b
}

pub fn ctx(caller: &AccountId, ts_sec: u64) -> VMContext {
    ctx_builder(caller, ts_sec).build()
}

/// Context with an attached deposit (for `fund`).
pub fn fund_ctx(caller: &AccountId, ts_sec: u64, deposit_yocto: u128) -> VMContext {
    ctx_builder(caller, ts_sec)
        .attached_deposit(NearToken::from_yoctonear(deposit_yocto))
        .build()
}

/// Sets up the mocked environment for invoking the private callback:
/// predecessor is the contract itself (as the `#[private]` wasm check
/// will require in production) and the given promise results are
/// injected.
pub fn callback_env(ts_sec: u64, results: Vec<PromiseResult>) {
    let context = ctx_builder(&contract_acct(), ts_sec).build();
    testing_env!(
        context,
        near_sdk::test_vm_config(),
        RuntimeFeesConfig::test(),
        Default::default(),
        results
    );
}

// ---------------------------------------------------------------------------
// Contract fixtures
// ---------------------------------------------------------------------------

/// Fresh contract with the default test policy:
/// max payment 5 NEAR, daily limit 20 NEAR, reserve floor 2 NEAR,
/// network id "sandbox".
pub fn init_contract(ts_sec: u64) -> Contract {
    testing_env!(ctx(&admin_acct(), ts_sec));
    Contract::new(
        admin_acct(),
        agent_acct(),
        "sandbox".to_string(),
        crate::types::U128(yocto(5)),
        crate::types::U128(yocto(20)),
        crate::types::U128(yocto(2)),
    )
}

/// Credits the internal ledger via a payable `fund` call.
pub fn fund_contract(contract: &mut Contract, ts_sec: u64, amount_yocto: u128) {
    testing_env!(fund_ctx(&admin_acct(), ts_sec, amount_yocto));
    contract.fund();
}

/// Standard fixture: initialized contract, 100 NEAR ledger, default
/// vendor registered with [`vendor_key`] and payout [`payout_acct`].
pub fn setup(ts_sec: u64) -> (Contract, SigningKey) {
    let mut contract = init_contract(ts_sec);
    fund_contract(&mut contract, ts_sec, yocto(100));
    testing_env!(ctx(&admin_acct(), ts_sec));
    contract.register_vendor(vendor_acct(), pk_of(&vendor_key()), payout_acct());
    (contract, vendor_key())
}

// ---------------------------------------------------------------------------
// Invoices
// ---------------------------------------------------------------------------

/// Build an invoice with explicit fields.
pub fn invoice(
    vendor_id: AccountId,
    nonce: u64,
    recipient: AccountId,
    amount_yocto: u128,
    expiry_sec: u64,
) -> SignedInvoice {
    SignedInvoice {
        protocol_id: crate::config::PROTOCOL_ID.to_string(),
        protocol_version: crate::config::PROTOCOL_VERSION,
        network_id: "sandbox".to_string(),
        contract_id: contract_acct(),
        vendor_id,
        nonce,
        recipient,
        amount: crate::types::U128(amount_yocto),
        expiration_unix_sec: expiry_sec,
    }
}

/// Valid default-vendor invoice whose expiry is one hour after `ts_sec`.
pub fn invoice_valid_at(nonce: u64, amount_yocto: u128, ts_sec: u64) -> SignedInvoice {
    invoice(vendor_acct(), nonce, payout_acct(), amount_yocto, ts_sec + 3600)
}

/// Sign an invoice with an Ed25519 key.
pub fn sign(key: &SigningKey, inv: &SignedInvoice) -> near_sdk::json_types::Base64VecU8 {
    near_sdk::json_types::Base64VecU8(key.sign(&inv.canonical_bytes()).to_bytes().to_vec())
}

// ---------------------------------------------------------------------------
// Payment operations
// ---------------------------------------------------------------------------

/// Pays a default-vendor invoice (as the agent) and returns the payment
/// id. Panics propagate to the caller (for `#[should_panic]` tests).
pub fn pay(contract: &mut Contract, key: &SigningKey, nonce: u64, amount_yocto: u128, ts_sec: u64) -> u64 {
    testing_env!(ctx(&agent_acct(), ts_sec));
    let inv = invoice_valid_at(nonce, amount_yocto, ts_sec);
    let sig = sign(key, &inv);
    let _ = contract.pay_invoice(inv, sig);
    contract.next_payment_id - 1
}

/// Invokes the private transfer callback as the contract itself with one
/// promise result.
pub fn finalize(contract: &mut Contract, payment_id: u64, ts_sec: u64, success: bool) {
    callback_env(
        ts_sec,
        vec![if success {
            PromiseResult::Successful(vec![])
        } else {
            PromiseResult::Failed
        }],
    );
    contract.on_transfer_complete(payment_id);
}

/// Submits a manual payment request as the agent.
pub fn request(
    contract: &mut Contract,
    ts_sec: u64,
    amount_yocto: u128,
    memo: Option<&str>,
) -> u64 {
    testing_env!(ctx(&agent_acct(), ts_sec));
    contract.request_manual_payment(
        vendor_acct(),
        crate::types::U128(amount_yocto),
        memo.map(|m| m.to_string()),
    )
}

/// Minimal hex decoder (test vectors only).
pub fn hex(s: &str) -> Vec<u8> {
    assert!(s.len() % 2 == 0);
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}
