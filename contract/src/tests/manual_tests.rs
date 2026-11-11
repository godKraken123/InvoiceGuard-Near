//! Manual payment request path: queueing, immutability of stored
//! requests, expiry, and the approval/rejection lifecycle.

use super::*;
use crate::types::{PaymentKind, PaymentStatus};

const D0: u64 = T0_SEC / DAY;

#[test]
fn manual_request_lifecycle() {
    let (mut contract, _key) = setup(T0_SEC);

    let id = request(&mut contract, T0_SEC, yocto(1), Some("coffee beans"));

    // Stored immutable, pending, with an expiry deadline.
    let p = contract.payments.get(&id).unwrap();
    assert_eq!(p.status, PaymentStatus::PendingApproval);
    assert_eq!(p.recipient, None, "recipient bound only at approval");
    assert_eq!(p.amount, yocto(1));
    assert_eq!(p.expires_at_sec, Some(T0_SEC + crate::config::DEFAULT_REQUEST_TTL_SEC));
    match &p.kind {
        PaymentKind::ManualRequest { memo } => assert_eq!(memo.as_deref(), Some("coffee beans")),
        k => panic!("unexpected kind: {k:?}"),
    }
    assert_eq!(contract.pending_request_ids.len(), 1);

    let logs = near_sdk::test_utils::get_logs();
    assert!(logs.iter().any(|l| l.contains("manual_payment_requested")), "logs: {logs:?}");

    // Approve as admin.
    testing_env!(ctx(&admin_acct(), T0_SEC + 10));
    contract.approve_manual_payment(id);

    let p = contract.payments.get(&id).unwrap();
    assert_eq!(p.status, PaymentStatus::Processing);
    assert_eq!(p.recipient, Some(payout_acct()), "recipient bound to registered payout");
    assert_eq!(p.expires_at_sec, None);
    assert_eq!(p.accounting_day, D0);
    assert!(contract.pending_request_ids.iter().all(|i| i != id));
    assert_eq!(contract.pending_request_ids.len(), 0);
    assert_eq!(contract.total_reserved, yocto(1));

    let logs = near_sdk::test_utils::get_logs();
    assert!(logs.iter().any(|l| l.contains("manual_payment_approved")), "logs: {logs:?}");

    // Finalize.
    finalize(&mut contract, id, T0_SEC + 20, true);
    assert_eq!(contract.payments.get(&id).unwrap().status, PaymentStatus::Paid);
    assert_eq!(contract.ledger_balance, yocto(99));
}

/// Approval executes exactly the stored fields: the only argument is
/// the request id, so nobody can alter amount, vendor, or memo between
/// request and approval.
#[test]
fn approval_executes_exactly_the_stored_fields() {
    let (mut contract, _key) = setup(T0_SEC);
    let requested = yocto(3);
    let id = request(&mut contract, T0_SEC, requested, Some("exact amount"));
    testing_env!(ctx(&admin_acct(), T0_SEC + 10));
    contract.approve_manual_payment(id);
    finalize(&mut contract, id, T0_SEC + 20, true);
    assert_eq!(contract.ledger_balance, yocto(100) - requested, "paid exactly the stored amount");
}

#[test]
fn rejection_discards_the_request() {
    let (mut contract, _key) = setup(T0_SEC);
    let id = request(&mut contract, T0_SEC, yocto(1), None);
    testing_env!(ctx(&admin_acct(), T0_SEC + 10));
    contract.reject_manual_payment(id);
    assert_eq!(contract.payments.get(&id).unwrap().status, PaymentStatus::Rejected);
    assert_eq!(contract.pending_request_ids.len(), 0);
    assert_eq!(contract.ledger_balance, yocto(100));
    assert_eq!(contract.total_reserved, 0);
}

#[test]
#[should_panic(expected = "E_REQUEST_EXPIRED")]
fn approving_an_expired_request_fails() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.set_request_ttl_seconds(60);
    let id = request(&mut contract, T0_SEC, yocto(1), None);
    testing_env!(ctx(&admin_acct(), T0_SEC + 61));
    contract.approve_manual_payment(id);
}

/// Rejecting an expired request is allowed (an explicit way to discard
/// it) as long as the lazy cleanup has not already expired it.
#[test]
fn rejecting_an_expired_request_is_allowed() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.set_request_ttl_seconds(60);
    let id = request(&mut contract, T0_SEC, yocto(1), None);
    testing_env!(ctx(&admin_acct(), T0_SEC + 61));
    contract.reject_manual_payment(id);
    assert_eq!(contract.payments.get(&id).unwrap().status, PaymentStatus::Rejected);
}

/// New requests lazily expire stale entries and free their queue slots.
#[test]
fn expiry_cleanup_marks_expired_and_frees_the_queue() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.set_request_ttl_seconds(60);

    let id_a = request(&mut contract, T0_SEC, yocto(1), None);
    let id_b = request(&mut contract, T0_SEC + 30, yocto(1), None);
    assert_eq!(contract.pending_request_ids.len(), 2);

    // A third request arrives after both deadlines: the first two are
    // lazily expired (B's deadline passes at T0+90; arrive at T0+120).
    let _id_c = request(&mut contract, T0_SEC + 120, yocto(1), None);

    assert_eq!(contract.payments.get(&id_a).unwrap().status, PaymentStatus::Expired);
    assert_eq!(contract.payments.get(&id_b).unwrap().status, PaymentStatus::Expired);
    assert_eq!(contract.pending_request_ids.len(), 1);

    let logs = near_sdk::test_utils::get_logs();
    assert!(logs.iter().any(|l| l.contains("manual_payment_expired")), "logs: {logs:?}");
}

/// Once the lazy cleanup has marked a request EXPIRED, it can no longer
/// be rejected (it is already terminal).
#[test]
#[should_panic(expected = "E_NOT_PENDING")]
fn rejecting_an_already_expired_request_fails() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.set_request_ttl_seconds(60);
    let id = request(&mut contract, T0_SEC, yocto(1), None);
    // Trigger the lazy expiry.
    let _other = request(&mut contract, T0_SEC + 120, yocto(1), None);
    testing_env!(ctx(&admin_acct(), T0_SEC + 130));
    contract.reject_manual_payment(id);
}

#[test]
#[should_panic(expected = "E_QUEUE_FULL")]
fn pending_queue_is_bounded() {
    let (mut contract, _key) = setup(T0_SEC);
    for _ in 0..crate::config::MAX_PENDING_REQUESTS {
        let _ = request(&mut contract, T0_SEC, yocto(1), None);
    }
    let _overflow = request(&mut contract, T0_SEC, yocto(1), None);
}

#[test]
fn rejecting_a_request_frees_a_queue_slot() {
    let (mut contract, _key) = setup(T0_SEC);
    let first = request(&mut contract, T0_SEC, yocto(1), None);
    for _ in 1..crate::config::MAX_PENDING_REQUESTS {
        let _ = request(&mut contract, T0_SEC, yocto(1), None);
    }
    assert_eq!(contract.pending_request_ids.len() as u64, crate::config::MAX_PENDING_REQUESTS);

    testing_env!(ctx(&admin_acct(), T0_SEC + 10));
    contract.reject_manual_payment(first);

    let _again = request(&mut contract, T0_SEC + 20, yocto(1), None);
    assert_eq!(contract.pending_request_ids.len() as u64, crate::config::MAX_PENDING_REQUESTS);
}

#[test]
#[should_panic(expected = "E_ZERO_AMOUNT")]
fn zero_amount_request_is_rejected() {
    let (mut contract, _key) = setup(T0_SEC);
    let _ = request(&mut contract, T0_SEC, 0, None);
}

#[test]
#[should_panic(expected = "E_MEMO_TOO_LONG")]
fn overlong_memo_is_rejected() {
    let (mut contract, _key) = setup(T0_SEC);
    let _ = request(&mut contract, T0_SEC, yocto(1), Some(&"x".repeat(257)));
}

#[test]
fn max_length_memo_is_accepted() {
    let (mut contract, _key) = setup(T0_SEC);
    let id = request(&mut contract, T0_SEC, yocto(1), Some(&"x".repeat(crate::config::MAX_MEMO_LEN)));
    match &contract.payments.get(&id).unwrap().kind {
        PaymentKind::ManualRequest { memo } => assert_eq!(memo.as_ref().map(|m| m.len()), Some(256)),
        k => panic!("unexpected kind: {k:?}"),
    }
}

#[test]
#[should_panic(expected = "E_TTL_TOO_LONG")]
fn request_ttl_has_an_upper_bound() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.set_request_ttl_seconds(crate::config::MAX_REQUEST_TTL_SEC + 1);
}

#[test]
fn approval_revalidates_vendor_status() {
    let (mut contract, _key) = setup(T0_SEC);
    let id = request(&mut contract, T0_SEC, yocto(1), None);

    // Vendor deactivated: approval fails, request stays pending.
    testing_env!(ctx(&admin_acct(), T0_SEC + 10));
    contract.set_vendor_active(vendor_acct(), false);
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        testing_env!(ctx(&admin_acct(), T0_SEC + 11));
        contract.approve_manual_payment(id);
    }));
    std::panic::set_hook(prev_hook);
    assert!(result.is_err());
    assert_eq!(contract.payments.get(&id).unwrap().status, PaymentStatus::PendingApproval);
    assert_eq!(contract.pending_request_ids.len(), 1);

    // Reactivate: now approval succeeds.
    testing_env!(ctx(&admin_acct(), T0_SEC + 12));
    contract.set_vendor_active(vendor_acct(), true);
    testing_env!(ctx(&admin_acct(), T0_SEC + 13));
    contract.approve_manual_payment(id);
    assert_eq!(contract.payments.get(&id).unwrap().status, PaymentStatus::Processing);
}

#[test]
#[should_panic(expected = "E_VENDOR_NOT_FOUND")]
fn approving_for_an_unregistered_vendor_fails() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&agent_acct(), T0_SEC));
    let id = contract.request_manual_payment(
        vendor2_acct(),
        crate::types::U128(yocto(1)),
        None,
    );
    testing_env!(ctx(&admin_acct(), T0_SEC + 10));
    contract.approve_manual_payment(id);
}

#[test]
#[should_panic(expected = "E_NOT_PENDING")]
fn approving_a_non_pending_payment_fails() {
    let (mut contract, key) = setup(T0_SEC);
    let id = pay(&mut contract, &key, 1, yocto(1), T0_SEC); // PROCESSING, not pending
    testing_env!(ctx(&admin_acct(), T0_SEC + 10));
    contract.approve_manual_payment(id);
}

#[test]
#[should_panic(expected = "E_PAYMENT_NOT_FOUND")]
fn approving_an_unknown_request_fails() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.approve_manual_payment(999);
}

/// A manual approval booked right before midnight finalizes after
/// midnight against its original day.
#[test]
fn manual_approval_books_to_the_approval_day() {
    let (mut contract, _key) = setup(T0_SEC);
    // Request late on day D0, approve early on day D0+1.
    let late_ts = (D0 + 1) * DAY - 100;
    let id = request(&mut contract, late_ts, yocto(1), None);

    let next_ts = (D0 + 1) * DAY + 100;
    testing_env!(ctx(&admin_acct(), next_ts));
    contract.approve_manual_payment(id);

    assert_eq!(contract.payments.get(&id).unwrap().accounting_day, D0 + 1);
    assert_eq!(contract.daily.get(&(D0 + 1)).unwrap().reserved, yocto(1));

    finalize(&mut contract, id, next_ts + 10, true);
    assert_eq!(contract.daily.get(&(D0 + 1)).unwrap().spent, yocto(1));
    assert_eq!(contract.ledger_balance, yocto(99));
}
