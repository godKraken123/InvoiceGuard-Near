//! Vendor-invoice payment path: authorization checks, replay
//! protection, treasury policy, and the transfer-finalization callback.

use super::*;
use crate::types::{PaymentKind, PaymentStatus, U128};

const D0: u64 = T0_SEC / DAY; // 20486

#[test]
fn happy_path_pays_and_books_correctly() {
    let (mut contract, key) = setup(T0_SEC);

    let id = pay(&mut contract, &key, 1, yocto(1), T0_SEC);

    // Payment record.
    let p = contract.payments.get(&id).unwrap();
    assert_eq!(p.status, PaymentStatus::Processing);
    assert_eq!(p.recipient, Some(payout_acct()));
    assert_eq!(p.vendor_id, vendor_acct());
    assert_eq!(p.amount, yocto(1));
    assert_eq!(p.accounting_day, D0);
    match &p.kind {
        PaymentKind::VendorInvoice { nonce } => assert_eq!(*nonce, 1),
        k => panic!("unexpected kind: {k:?}"),
    }

    // Nonce consumed immediately.
    assert_eq!(contract.vendors.get(&vendor_acct()).unwrap().next_nonce, 2);

    // Funds reserved.
    assert_eq!(contract.total_reserved, yocto(1));
    assert_eq!(contract.daily.get(&D0).unwrap().reserved, yocto(1));
    assert_eq!(contract.ledger_balance, yocto(100));
    // available = ledger - reserved - floor - headroom = 100-1-2-1
    assert_eq!(contract.available_funds(), yocto(96));

    // Events were emitted (NEP-297).
    let logs = near_sdk::test_utils::get_logs();
    assert!(logs.iter().any(|l| l.contains("invoice_accepted")), "logs: {logs:?}");

    // Transfer succeeded.
    finalize(&mut contract, id, T0_SEC + 10, true);

    let p = contract.payments.get(&id).unwrap();
    assert_eq!(p.status, PaymentStatus::Paid);
    assert_eq!(contract.ledger_balance, yocto(99));
    assert_eq!(contract.total_reserved, 0);
    let day = contract.daily.get(&D0).unwrap();
    assert_eq!(day.spent, yocto(1));
    assert_eq!(day.reserved, 0);
    // available = ledger - reserved - floor - headroom = 99-0-2-1
    assert_eq!(contract.available_funds(), yocto(96));

    let logs = near_sdk::test_utils::get_logs();
    assert!(logs.iter().any(|l| l.contains("payment_paid")), "logs: {logs:?}");
}

#[test]
fn double_finalization_is_rejected() {
    let (mut contract, key) = setup(T0_SEC);
    let id = pay(&mut contract, &key, 1, yocto(1), T0_SEC);
    finalize(&mut contract, id, T0_SEC + 10, true);
    // Second callback for the same payment: must not finalize twice.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        finalize(&mut contract, id, T0_SEC + 20, true);
    }));
    std::panic::set_hook(prev_hook);
    assert!(result.is_err());
    // Books are unchanged: paid exactly once.
    assert_eq!(contract.ledger_balance, yocto(99));
    assert_eq!(contract.payments.get(&id).unwrap().status, PaymentStatus::Paid);
}

#[test]
#[should_panic(expected = "E_BAD_SIGNATURE")]
fn forged_signature_is_rejected() {
    let (mut contract, _key) = setup(T0_SEC);
    let inv = invoice_valid_at(1, yocto(1), T0_SEC);
    let sig = sign(&attacker_key(), &inv);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    let _ = contract.pay_invoice(inv, sig);
}

#[test]
#[should_panic(expected = "E_BAD_SIGNATURE")]
fn tampered_amount_is_rejected() {
    let (mut contract, key) = setup(T0_SEC);
    // Signed for 1 NEAR, submitted for 2 NEAR.
    let signed = invoice_valid_at(1, yocto(1), T0_SEC);
    let sig = sign(&key, &signed);
    let mut submitted = signed;
    submitted.amount = crate::types::U128(yocto(2));
    testing_env!(ctx(&agent_acct(), T0_SEC));
    let _ = contract.pay_invoice(submitted, sig);
}

#[test]
#[should_panic(expected = "E_SIG_LEN")]
fn malformed_signature_length_is_rejected() {
    let (mut contract, _key) = setup(T0_SEC);
    let inv = invoice_valid_at(1, yocto(1), T0_SEC);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    let _ = contract.pay_invoice(inv, near_sdk::json_types::Base64VecU8(vec![0u8; 63]));
}

#[test]
#[should_panic(expected = "E_INVOICE_EXPIRED")]
fn expired_invoice_is_rejected() {
    let (mut contract, key) = setup(T0_SEC);
    // Expires exactly now: payments must be strictly before the expiry.
    let inv = invoice(vendor_acct(), 1, payout_acct(), yocto(1), T0_SEC);
    let sig = sign(&key, &inv);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    let _ = contract.pay_invoice(inv, sig);
}

#[test]
#[should_panic(expected = "E_NONCE_MISMATCH")]
fn nonce_skipping_is_rejected() {
    let (mut contract, key) = setup(T0_SEC);
    let inv = invoice_valid_at(2, yocto(1), T0_SEC);
    let sig = sign(&key, &inv);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    let _ = contract.pay_invoice(inv, sig);
}

#[test]
#[should_panic(expected = "E_NONCE_MISMATCH")]
fn replaying_the_same_invoice_is_rejected() {
    let (mut contract, key) = setup(T0_SEC);
    let id = pay(&mut contract, &key, 1, yocto(1), T0_SEC);
    finalize(&mut contract, id, T0_SEC + 10, true);
    // Same invoice, same signature, again.
    let inv = invoice_valid_at(1, yocto(1), T0_SEC);
    let sig = sign(&key, &inv);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    let _ = contract.pay_invoice(inv, sig);
}

/// A failed transfer must NOT roll back the nonce: the very same
/// invoice can never trigger a second transfer. A retry requires a
/// newly signed invoice with the next nonce.
#[test]
fn nonce_is_not_rolled_back_after_failure() {
    let (mut contract, key) = setup(T0_SEC);
    let id = pay(&mut contract, &key, 1, yocto(1), T0_SEC);
    finalize(&mut contract, id, T0_SEC + 10, false);

    assert_eq!(contract.payments.get(&id).unwrap().status, PaymentStatus::Failed);
    assert_eq!(contract.vendors.get(&vendor_acct()).unwrap().next_nonce, 2);

    // Replaying the failed invoice is rejected.
    let inv = invoice_valid_at(1, yocto(1), T0_SEC);
    let sig = sign(&key, &inv);
    testing_env!(ctx(&agent_acct(), T0_SEC + 20));
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = contract.pay_invoice(inv.clone(), sig);
    }));
    std::panic::set_hook(prev_hook);
    assert!(result.is_err());

    // A newly signed invoice with the next nonce succeeds.
    let id2 = pay(&mut contract, &key, 2, yocto(1), T0_SEC + 30);
    finalize(&mut contract, id2, T0_SEC + 40, true);
    assert_eq!(contract.payments.get(&id2).unwrap().status, PaymentStatus::Paid);
    assert_eq!(contract.ledger_balance, yocto(99));
}

#[test]
fn failed_transfer_releases_the_reservation_and_keeps_the_ledger() {
    let (mut contract, key) = setup(T0_SEC);
    let id = pay(&mut contract, &key, 1, yocto(1), T0_SEC);
    finalize(&mut contract, id, T0_SEC + 10, false);

    assert_eq!(contract.payments.get(&id).unwrap().status, PaymentStatus::Failed);
    assert_eq!(contract.ledger_balance, yocto(100), "ledger must be unchanged");
    assert_eq!(contract.total_reserved, 0);
    let day = contract.daily.get(&D0).unwrap();
    assert_eq!(day.spent, 0);
    assert_eq!(day.reserved, 0);

    let logs = near_sdk::test_utils::get_logs();
    assert!(logs.iter().any(|l| l.contains("payment_failed")), "logs: {logs:?}");
}

/// Recipient binding is enforced independently of the signature: even a
/// perfectly valid signature over a different recipient is refused,
/// because the registered payout account is the only legal recipient.
#[test]
#[should_panic(expected = "E_RECIPIENT_MISMATCH")]
fn recipient_binding_is_enforced_even_with_valid_signature() {
    let (mut contract, key) = setup(T0_SEC);
    let inv = invoice(vendor_acct(), 1, payout2_acct(), yocto(1), T0_SEC + 3600);
    let sig = sign(&key, &inv);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    let _ = contract.pay_invoice(inv, sig);
}

/// A vendor's invoice must be signed by that vendor's registered key.
/// (Recipient is vendor2's payout so the recipient check passes and the
/// signature check itself is exercised.)
#[test]
#[should_panic(expected = "E_BAD_SIGNATURE")]
fn cross_vendor_signature_is_rejected() {
    let (mut contract, key) = setup(T0_SEC);
    // Register a second vendor with its own key.
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.register_vendor(vendor2_acct(), pk_of(&vendor2_key()), payout2_acct());

    // Invoice for vendor2, signed by vendor1's key.
    let inv = invoice(vendor2_acct(), 1, payout2_acct(), yocto(1), T0_SEC + 3600);
    let sig = sign(&key, &inv);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    let _ = contract.pay_invoice(inv, sig);
}

#[test]
#[should_panic(expected = "E_VENDOR_NOT_FOUND")]
fn unknown_vendor_is_rejected() {
    let (mut contract, key) = setup(T0_SEC);
    let inv = invoice(vendor2_acct(), 1, payout2_acct(), yocto(1), T0_SEC + 3600);
    let sig = sign(&key, &inv);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    let _ = contract.pay_invoice(inv, sig);
}

#[test]
#[should_panic(expected = "E_ZERO_AMOUNT")]
fn zero_amount_invoice_is_rejected() {
    let (mut contract, key) = setup(T0_SEC);
    let inv = invoice_valid_at(1, 0, T0_SEC);
    let sig = sign(&key, &inv);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    let _ = contract.pay_invoice(inv, sig);
}

#[test]
#[should_panic(expected = "E_OVER_MAX_PAYMENT")]
fn per_payment_maximum_is_enforced() {
    let (mut contract, key) = setup(T0_SEC);
    pay(&mut contract, &key, 1, yocto(6), T0_SEC); // max is 5 NEAR
}

#[test]
#[should_panic(expected = "E_OVER_DAILY_LIMIT")]
fn daily_limit_counts_spent_and_reserved() {
    let (mut contract, key) = setup(T0_SEC);
    // Raise the per-payment cap so the daily limit is the binding constraint
    // (the default 5 NEAR cap would reject the very first 6 NEAR payment).
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.set_max_payment(U128(yocto(10)));
    // Daily limit is 20 NEAR.
    let id1 = pay(&mut contract, &key, 1, yocto(6), T0_SEC);
    finalize(&mut contract, id1, T0_SEC + 10, true);
    let id2 = pay(&mut contract, &key, 2, yocto(6), T0_SEC + 20);
    finalize(&mut contract, id2, T0_SEC + 30, true);
    let id3 = pay(&mut contract, &key, 3, yocto(6), T0_SEC + 40);
    finalize(&mut contract, id3, T0_SEC + 50, true);
    // 18 NEAR spent today; the next 6 would exceed the 20 NEAR limit.
    pay(&mut contract, &key, 4, yocto(6), T0_SEC + 60);
}

/// Reservations count against the daily limit while in flight: two
/// concurrent 6 NEAR payments (12 reserved) + a third exceeds the limit
/// even though nothing has finalized yet.
#[test]
#[should_panic(expected = "E_OVER_DAILY_LIMIT")]
fn reservations_count_against_the_daily_limit() {
    let (mut contract, key) = setup(T0_SEC);
    // Raise the per-payment cap so the daily limit is the binding constraint.
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.set_max_payment(U128(yocto(10)));
    pay(&mut contract, &key, 1, yocto(6), T0_SEC);
    pay(&mut contract, &key, 2, yocto(6), T0_SEC + 10);
    pay(&mut contract, &key, 3, yocto(6), T0_SEC + 20);
    pay(&mut contract, &key, 4, yocto(6), T0_SEC + 30);
    // 24 NEAR reserved > 20 NEAR limit.
}

#[test]
#[should_panic(expected = "E_INSUFFICIENT_FUNDS")]
fn ledger_must_cover_payment_over_floor_and_headroom() {
    // Fund only 4 NEAR: available = 4 - 2 (floor) - 1 (headroom) = 1 NEAR.
    let mut contract = init_contract(T0_SEC);
    fund_contract(&mut contract, T0_SEC, yocto(4));
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.register_vendor(vendor_acct(), pk_of(&vendor_key()), payout_acct());

    let inv = invoice_valid_at(1, yocto(2), T0_SEC);
    let sig = sign(&vendor_key(), &inv);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    let _ = contract.pay_invoice(inv, sig);
}

/// Defense in depth: even when the internal ledger would allow the
/// payment, the contract's physical unlocked account balance must cover
/// in-flight transfers plus the operations headroom.
#[test]
#[should_panic(expected = "E_PHYSICAL_FUNDS")]
fn physical_balance_is_checked() {
    let (mut contract, key) = setup(T0_SEC);
    let mut b = ctx_builder(&agent_acct(), T0_SEC);
    b.account_balance(NearToken::from_near(1)); // 1 NEAR unlocked
    testing_env!(b.build());
    let inv = invoice_valid_at(1, yocto(1), T0_SEC);
    let sig = sign(&key, &inv);
    let _ = contract.pay_invoice(inv, sig);
}

#[test]
#[should_panic(expected = "E_PAUSED")]
fn paused_contract_refuses_payments() {
    let (mut contract, key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.pause();
    pay(&mut contract, &key, 1, yocto(1), T0_SEC);
}

#[test]
fn unpausing_restores_payments() {
    let (mut contract, key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.pause();
    testing_env!(ctx(&admin_acct(), T0_SEC + 1));
    contract.unpause();
    let id = pay(&mut contract, &key, 1, yocto(1), T0_SEC + 2);
    finalize(&mut contract, id, T0_SEC + 3, true);
    assert_eq!(contract.payments.get(&id).unwrap().status, PaymentStatus::Paid);
}

/// A payment started before midnight and finalized after midnight is
/// booked against its original UTC day.
#[test]
fn midnight_crossing_books_to_the_original_day() {
    let (mut contract, key) = setup(T0_SEC);
    let id = pay(&mut contract, &key, 1, yocto(1), T0_SEC);

    // Finalize well into the next day.
    let next_day_ts = (D0 + 1) * DAY + 100;
    finalize(&mut contract, id, next_day_ts, true);

    assert_eq!(contract.payments.get(&id).unwrap().accounting_day, D0);
    let day = contract.daily.get(&D0).unwrap();
    assert_eq!(day.spent, yocto(1));
    assert_eq!(day.reserved, 0);
    // The callback itself does not roll the daily window.
    assert_eq!(contract.current_day, D0);

    // The next payment rolls the window and prunes the finished day.
    let id2 = pay(&mut contract, &key, 2, yocto(1), next_day_ts);
    assert_eq!(contract.current_day, D0 + 1);
    assert!(
        contract.daily.get(&D0).is_none(),
        "finished day entry should be pruned"
    );
    assert_eq!(contract.daily.get(&(D0 + 1)).unwrap().reserved, yocto(1));
    finalize(&mut contract, id2, next_day_ts + 10, true);
    assert_eq!(contract.daily.get(&(D0 + 1)).unwrap().spent, yocto(1));
}

/// Days with outstanding reservations are never pruned, so a
/// cross-midnight finalization can always find its day entry.
#[test]
fn days_with_reservations_are_not_pruned() {
    let (mut contract, key) = setup(T0_SEC);
    let id = pay(&mut contract, &key, 1, yocto(1), T0_SEC);

    // Roll the window forward while the reservation is still open.
    let next_day_ts = (D0 + 1) * DAY + 100;
    testing_env!(ctx(&admin_acct(), next_day_ts));
    let _ = contract.get_config(); // no roll: views don't mutate
    let id2 = pay(&mut contract, &key, 2, yocto(1), next_day_ts); // rolls to D0+1

    assert!(contract.daily.get(&D0).is_some(), "day with open reservation must survive");
    assert_eq!(contract.daily.get(&D0).unwrap().reserved, yocto(1));

    // Finalize the old payment afterwards.
    finalize(&mut contract, id, next_day_ts + 10, true);
    assert_eq!(contract.daily.get(&D0).unwrap().spent, yocto(1));
    assert_eq!(contract.daily.get(&D0).unwrap().reserved, 0);
    finalize(&mut contract, id2, next_day_ts + 20, true);
}

#[test]
#[should_panic(expected = "E_CALLBACK_COUNT")]
fn callback_requires_exactly_one_promise_result() {
    let (mut contract, key) = setup(T0_SEC);
    let id = pay(&mut contract, &key, 1, yocto(1), T0_SEC);
    callback_env(T0_SEC + 10, vec![]); // zero results
    contract.on_transfer_complete(id);
}

/// An oversized/unexpected promise result fails closed: accounting is
/// untouched, the payment stays PROCESSING, and a clean retry
/// finalizes correctly.
#[test]
fn oversized_promise_result_fails_closed_and_can_be_retried() {
    let (mut contract, key) = setup(T0_SEC);
    let id = pay(&mut contract, &key, 1, yocto(1), T0_SEC);

    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        callback_env(T0_SEC + 10, vec![PromiseResult::Successful(vec![0u8; 64])]);
        contract.on_transfer_complete(id);
    }));
    std::panic::set_hook(prev_hook);
    assert!(result.is_err());

    // Nothing was mutated.
    assert_eq!(contract.payments.get(&id).unwrap().status, PaymentStatus::Processing);
    assert_eq!(contract.total_reserved, yocto(1));
    assert_eq!(contract.ledger_balance, yocto(100));

    // Clean retry works.
    finalize(&mut contract, id, T0_SEC + 20, true);
    assert_eq!(contract.payments.get(&id).unwrap().status, PaymentStatus::Paid);
    assert_eq!(contract.ledger_balance, yocto(99));
}

#[test]
#[should_panic(expected = "E_PAYMENT_NOT_FOUND")]
fn callback_for_unknown_payment_is_rejected() {
    let (mut contract, _key) = setup(T0_SEC);
    finalize(&mut contract, 999, T0_SEC + 10, true);
}

/// History retention: terminal payments beyond the 100-record window
/// are deleted, while replay protection (vendor nonces) is unaffected.
#[test]
fn history_retention_trims_terminal_payments_only() {
    let mut contract = init_contract(T0_SEC);
    fund_contract(&mut contract, T0_SEC, yocto(1000));
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.register_vendor(vendor_acct(), pk_of(&vendor_key()), payout_acct());
    // 105 x 1 NEAR exceeds the default 20 NEAR daily cap; raise it so this
    // test exercises history trimming, not the daily limit.
    contract.set_daily_limit(U128(yocto(1000)));

    for i in 0..105u64 {
        let id = pay(&mut contract, &vendor_key(), i + 1, yocto(1), T0_SEC + i);
        finalize(&mut contract, id, T0_SEC + i, true);
    }

    assert_eq!(contract.payments.len(), crate::config::MAX_RETAINED_PAYMENTS);
    assert_eq!(contract.oldest_retained_payment_id, 5);
    assert!(contract.payments.get(&0).is_none());
    assert!(contract.payments.get(&4).is_none());
    assert!(contract.payments.get(&5).is_some());
    // Nonces kept counting: replay protection is independent of history.
    assert_eq!(contract.vendors.get(&vendor_acct()).unwrap().next_nonce, 106);
}
