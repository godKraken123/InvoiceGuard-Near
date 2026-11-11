//! Vendor registry lifecycle and how each registry change gates
//! payments.

use super::*;
use crate::types::PaymentStatus;

#[test]
#[should_panic(expected = "E_VENDOR_EXISTS")]
fn duplicate_vendor_registration_is_rejected() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.register_vendor(vendor_acct(), pk_of(&vendor_key()), payout_acct());
}

#[test]
#[should_panic(expected = "E_PAYOUT_IS_CONTRACT")]
fn payout_cannot_be_the_contract_itself() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.register_vendor(vendor2_acct(), pk_of(&vendor2_key()), contract_acct());
}

#[test]
#[should_panic(expected = "E_KEY_NOT_ED25519")]
fn non_ed25519_keys_are_rejected_at_registration() {
    let (mut contract, _key) = setup(T0_SEC);
    let secp = near_sdk::PublicKey::from_parts(
        near_sdk::CurveType::SECP256K1,
        vec![0u8; 64],
    )
    .unwrap();
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.register_vendor(vendor2_acct(), secp, payout2_acct());
}

#[test]
#[should_panic(expected = "E_VENDOR_NOT_FOUND")]
fn configuring_an_unknown_vendor_fails() {
    let (mut contract, _key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.set_vendor_active(vendor2_acct(), false);
}

#[test]
fn vendor_capacity_is_bounded() {
    let (mut contract, _key) = setup(T0_SEC);
    // One vendor (the default) is already registered.
    for i in 2..=crate::config::MAX_VENDORS {
        let id: near_sdk::AccountId = format!("vendor{i}.unit.test.near").parse().unwrap();
        testing_env!(ctx(&admin_acct(), T0_SEC));
        contract.register_vendor(id, pk_of(&vendor2_key()), payout2_acct());
    }
    assert_eq!(contract.vendor_ids.len() as u64, crate::config::MAX_VENDORS);

    let overflow: near_sdk::AccountId = "vendor65.unit.test.near".parse().unwrap();
    testing_env!(ctx(&admin_acct(), T0_SEC));
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        contract.register_vendor(overflow, pk_of(&vendor2_key()), payout2_acct());
    }));
    std::panic::set_hook(prev_hook);
    assert!(result.is_err(), "65th vendor must be rejected");
    assert_eq!(contract.vendor_ids.len() as u64, crate::config::MAX_VENDORS);
}

#[test]
#[should_panic(expected = "E_VENDOR_INACTIVE")]
fn inactive_vendor_invoices_are_refused() {
    let (mut contract, key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.set_vendor_active(vendor_acct(), false);
    pay(&mut contract, &key, 1, yocto(1), T0_SEC);
}

#[test]
fn reactivated_vendor_can_be_paid_again() {
    let (mut contract, key) = setup(T0_SEC);
    testing_env!(ctx(&admin_acct(), T0_SEC));
    contract.set_vendor_active(vendor_acct(), false);
    testing_env!(ctx(&admin_acct(), T0_SEC + 1));
    contract.set_vendor_active(vendor_acct(), true);
    let id = pay(&mut contract, &key, 1, yocto(1), T0_SEC + 2);
    finalize(&mut contract, id, T0_SEC + 3, true);
    assert_eq!(contract.payments.get(&id).unwrap().status, PaymentStatus::Paid);
}

#[test]
fn key_rotation_invalidates_old_signatures() {
    let (mut contract, key) = setup(T0_SEC);
    let id = pay(&mut contract, &key, 1, yocto(1), T0_SEC);
    finalize(&mut contract, id, T0_SEC + 10, true);

    // Rotate to a new key.
    testing_env!(ctx(&admin_acct(), T0_SEC + 20));
    contract.rotate_vendor_key(vendor_acct(), pk_of(&rotated_key()));

    // Old key, next nonce: refused.
    let old_inv = invoice_valid_at(2, yocto(1), T0_SEC + 30);
    let old_sig = sign(&key, &old_inv);
    testing_env!(ctx(&agent_acct(), T0_SEC + 30));
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = contract.pay_invoice(old_inv, old_sig);
    }));
    std::panic::set_hook(prev_hook);
    assert!(result.is_err());

    // New key, next nonce: accepted. Nonce sequence was NOT reset.
    let id2 = pay(&mut contract, &rotated_key(), 2, yocto(1), T0_SEC + 40);
    finalize(&mut contract, id2, T0_SEC + 50, true);
    assert_eq!(contract.payments.get(&id2).unwrap().status, PaymentStatus::Paid);
    assert_eq!(contract.vendors.get(&vendor_acct()).unwrap().next_nonce, 3);
}

#[test]
fn payout_change_binds_future_payments_only() {
    let (mut contract, key) = setup(T0_SEC);

    // In-flight payment to the old payout keeps its recipient.
    let id = pay(&mut contract, &key, 1, yocto(1), T0_SEC);

    // Admin moves the payout account.
    testing_env!(ctx(&admin_acct(), T0_SEC + 10));
    contract.set_vendor_payout(vendor_acct(), payout2_acct());

    let p = contract.payments.get(&id).unwrap();
    assert_eq!(p.recipient, Some(payout_acct()), "in-flight payment unchanged");

    // An invoice still naming the old payout is refused even when
    // correctly signed for it.
    let old_inv = invoice_valid_at(2, yocto(1), T0_SEC + 20);
    let old_sig = sign(&key, &old_inv);
    testing_env!(ctx(&agent_acct(), T0_SEC + 20));
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = contract.pay_invoice(old_inv, old_sig);
    }));
    std::panic::set_hook(prev_hook);
    assert!(result.is_err());

    // The new payout account is accepted for the next nonce.
    let new_inv = invoice(vendor_acct(), 2, payout2_acct(), yocto(1), T0_SEC + 3600);
    let new_sig = sign(&key, &new_inv);
    testing_env!(ctx(&agent_acct(), T0_SEC + 30));
    let _ = contract.pay_invoice(new_inv, new_sig);
    let id2 = contract.next_payment_id - 1;
    assert_eq!(contract.payments.get(&id2).unwrap().recipient, Some(payout2_acct()));
}
