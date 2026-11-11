//! Authorization matrix: who may call what.
//!
//! The core property: the *predecessor* account is the authorization
//! subject (never the transaction signer), the agent can spend only
//! within policy, and the admin can configure but never withdraw.

use super::*;
use crate::types::U128;

#[test]
#[should_panic(expected = "E_NOT_AGENT")]
fn admin_cannot_call_pay_invoice() {
    let (mut contract, key) = setup(T0_SEC);
    let inv = invoice_valid_at(1, yocto(1), T0_SEC);
    let sig = sign(&key, &inv);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    let _ = contract.pay_invoice(inv, sig);
}

#[test]
#[should_panic(expected = "E_NOT_AGENT")]
fn attacker_cannot_call_pay_invoice() {
    let (mut contract, key) = setup(T0_SEC);
    let inv = invoice_valid_at(1, yocto(1), T0_SEC);
    let sig = sign(&key, &inv);
    testing_env!(ctx(&attacker_acct(), T0_SEC));
    let _ = contract.pay_invoice(inv, sig);
}

#[test]
#[should_panic(expected = "E_NOT_AGENT")]
fn contract_account_cannot_call_pay_invoice() {
    let (mut contract, key) = setup(T0_SEC);
    let inv = invoice_valid_at(1, yocto(1), T0_SEC);
    let sig = sign(&key, &inv);
    testing_env!(ctx(&contract_acct(), T0_SEC));
    let _ = contract.pay_invoice(inv, sig);
}

#[test]
#[should_panic(expected = "E_NOT_AGENT")]
fn admin_cannot_submit_manual_requests() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.request_manual_payment(vendor_acct(), U128(yocto(1)), None);
}

#[test]
#[should_panic(expected = "E_NOT_AGENT")]
fn attacker_cannot_submit_manual_requests() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&attacker_acct(), T0_SEC));
    contract.request_manual_payment(vendor_acct(), U128(yocto(1)), None);
}

#[test]
#[should_panic(expected = "E_NOT_ADMIN")]
fn agent_cannot_approve_manual_requests() {
    let (mut contract, _key) = setup(T0_SEC);
    let id = request(&mut contract, T0_SEC, yocto(1), None);
    testing_env!(ctx(&agent_acct(), T0_SEC + 10));
    contract.approve_manual_payment(id);
}

#[test]
#[should_panic(expected = "E_NOT_ADMIN")]
fn attacker_cannot_approve_manual_requests() {
    let (mut contract, _key) = setup(T0_SEC);
    let id = request(&mut contract, T0_SEC, yocto(1), None);
    testing_env!(ctx(&attacker_acct(), T0_SEC + 10));
    contract.approve_manual_payment(id);
}

#[test]
#[should_panic(expected = "E_NOT_ADMIN")]
fn agent_cannot_reject_manual_requests() {
    let (mut contract, _key) = setup(T0_SEC);
    let id = request(&mut contract, T0_SEC, yocto(1), None);
    testing_env!(ctx(&agent_acct(), T0_SEC + 10));
    contract.reject_manual_payment(id);
}

/// The agent (the least-trusted operating account) must not be able to
/// touch any configuration knob.
#[test]
#[should_panic(expected = "E_NOT_ADMIN")]
fn agent_cannot_change_policy() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    contract.set_max_payment(U128(yocto(100)));
}

#[test]
#[should_panic(expected = "E_NOT_ADMIN")]
fn attacker_cannot_change_the_agent_account() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&attacker_acct(), T0_SEC));
    contract.set_agent(attacker_acct());
}

#[test]
#[should_panic(expected = "E_NOT_ADMIN")]
fn agent_cannot_pause() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    contract.pause();
}

#[test]
#[should_panic(expected = "E_NOT_ADMIN")]
fn attacker_cannot_register_vendors() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&attacker_acct(), T0_SEC));
    contract.register_vendor(attacker_acct(), pk_of(&attacker_key()), attacker_acct());
}

#[test]
#[should_panic(expected = "E_NOT_ADMIN")]
fn agent_cannot_rotate_vendor_keys() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    contract.rotate_vendor_key(vendor_acct(), pk_of(&rotated_key()));
}

#[test]
#[should_panic(expected = "E_NOT_ADMIN")]
fn agent_cannot_change_vendor_payouts() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    contract.set_vendor_payout(vendor_acct(), attacker_acct());
}

#[test]
#[should_panic(expected = "E_NOT_ADMIN")]
fn agent_cannot_set_vendor_active() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    contract.set_vendor_active(vendor_acct(), false);
}

#[test]
#[should_panic(expected = "E_NOT_ADMIN")]
fn agent_cannot_set_daily_limit() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    contract.set_daily_limit(U128(yocto(1000)));
}

#[test]
#[should_panic(expected = "E_NOT_ADMIN")]
fn agent_cannot_set_reserve_floor() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    contract.set_reserve_floor(U128(0));
}

#[test]
#[should_panic(expected = "E_NOT_ADMIN")]
fn agent_cannot_set_request_ttl() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    contract.set_request_ttl_seconds(1);
}

#[test]
#[should_panic(expected = "E_NOT_ADMIN")]
fn agent_cannot_unpause() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.pause();
    testing_env!(ctx(&agent_acct(), T0_SEC));
    contract.unpause();
}

#[test]
fn anyone_can_fund_the_treasury() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(fund_ctx(&attacker_acct(), T0_SEC, yocto(7)));
    contract.fund();
    assert_eq!(contract.ledger_balance, yocto(107));
}

#[test]
#[should_panic(expected = "E_ZERO_DEPOSIT")]
fn zero_deposit_fund_is_rejected() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(fund_ctx(&attacker_acct(), T0_SEC, 0));
    contract.fund();
}

// ---------------------------------------------------------------------------
// init validation
// ---------------------------------------------------------------------------

#[test]
#[should_panic(expected = "E_ADMIN_IS_AGENT")]
fn init_rejects_admin_eq_agent() {
    testing_env!(ctx(&admin_acct(), T0_SEC));
    let _ = Contract::new(
        admin_acct(),
        admin_acct(),
        "sandbox".to_string(),
        U128(yocto(5)),
        U128(yocto(20)),
        U128(yocto(2)),
    );
}

#[test]
#[should_panic(expected = "E_ZERO_MAX_PAYMENT")]
fn init_rejects_zero_max_payment() {
    testing_env!(ctx(&admin_acct(), T0_SEC));
    let _ = Contract::new(
        admin_acct(),
        agent_acct(),
        "sandbox".to_string(),
        U128(0),
        U128(yocto(20)),
        U128(yocto(2)),
    );
}

#[test]
#[should_panic(expected = "E_ZERO_DAILY_LIMIT")]
fn init_rejects_zero_daily_limit() {
    testing_env!(ctx(&admin_acct(), T0_SEC));
    let _ = Contract::new(
        admin_acct(),
        agent_acct(),
        "sandbox".to_string(),
        U128(yocto(5)),
        U128(0),
        U128(yocto(2)),
    );
}

#[test]
#[should_panic(expected = "E_EMPTY_NETWORK_ID")]
fn init_rejects_empty_network_id() {
    testing_env!(ctx(&admin_acct(), T0_SEC));
    let _ = Contract::new(
        admin_acct(),
        agent_acct(),
        "".to_string(),
        U128(yocto(5)),
        U128(yocto(20)),
        U128(yocto(2)),
    );
}

#[test]
#[should_panic(expected = "E_NETWORK_ID_TOO_LONG")]
fn init_rejects_overlong_network_id() {
    testing_env!(ctx(&admin_acct(), T0_SEC));
    let _ = Contract::new(
        admin_acct(),
        agent_acct(),
        "012345678901234567890123456789012".to_string(),
        U128(yocto(5)),
        U128(yocto(20)),
        U128(yocto(2)),
    );
}

#[test]
fn init_accepts_32_char_network_id() {
    testing_env!(ctx(&admin_acct(), T0_SEC));
    let contract = Contract::new(
        admin_acct(),
        agent_acct(),
        "01234567890123456789012345678901".to_string(),
        U128(yocto(5)),
        U128(yocto(20)),
        U128(yocto(2)),
    );
    assert_eq!(contract.network_id.len(), crate::config::MAX_NETWORK_ID_LEN);
}

#[test]
#[should_panic(expected = "E_ADMIN_IS_AGENT")]
fn set_agent_cannot_merge_admin_and_agent() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.set_agent(admin_acct());
}

#[test]
#[should_panic(expected = "E_ALREADY_PAUSED")]
fn pausing_twice_fails() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.pause();
    testing_env!(ctx(&admin_acct(), T0_SEC + 1));
    contract.pause();
}

#[test]
#[should_panic(expected = "E_NOT_PAUSED")]
fn unpausing_when_not_paused_fails() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.unpause();
}
