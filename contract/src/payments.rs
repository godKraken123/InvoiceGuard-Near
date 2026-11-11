//! Payment paths and the private transfer-finalization callback.
//!
//! # Vendor invoice path (`pay_invoice`)
//!
//! Checks run in this order, all *before* any state mutation:
//! caller is the agent; not paused; domain separation fields are valid;
//! amount > 0; vendor exists and is active; recipient is bound to the
//! registered payout account; invoice nonce equals the vendor's expected
//! next nonce; invoice is not expired; Ed25519 signature over the canonical
//! payload verifies against the *registered* vendor key; per-payment max;
//! daily limit including existing reservations; available funds.
//!
//! Only then: the nonce is consumed, a payment record is created in
//! PROCESSING state, funds are reserved, and the transfer + callback are
//! scheduled. A failed transfer does NOT roll the nonce back: a retry
//! requires a newly signed invoice with the next nonce.
//!
//! # Manual request path
//!
//! The agent submits immutable request fields; the contract generates the
//! request id. Nothing is reserved or paid until the administrator
//! approves the exact stored request, at which point *current* policy is
//! revalidated and the recipient is bound to the vendor's registered
//! payout account (the administrator cannot choose a recipient).

use near_sdk::json_types::{Base64VecU8, U128};
use near_sdk::{env, near_bindgen, require, AccountId, Gas, Promise, PromiseError};

use crate::config::*;
use crate::events::InvoiceGuardEvent;
use crate::invoice::SignedInvoice;
use crate::types::*;
use crate::{Contract, ContractExt};

/// Gas reserved for the private callback.
const CALLBACK_GAS: Gas = Gas::from_tgas(CALLBACK_GAS_TERAGAS);

#[near_bindgen]
impl Contract {
    // ------------------------------------------------------------------
    // Vendor-signed invoice path
    // ------------------------------------------------------------------

    /// Pay a vendor-signed invoice.
    ///
    /// Callable only by the configured agent account. See the module docs
    /// for the exact check order. On success the transfer is scheduled and
    /// finalized by the private [`Contract::on_transfer_complete`]
    /// callback.
    pub fn pay_invoice(&mut self, invoice: SignedInvoice, signature: Base64VecU8) -> Promise {
        require!(self.is_agent(), "E_NOT_AGENT");
        require!(!self.paused, "E_PAUSED");

        // Domain separation (protocol, version, network, contract).
        invoice
            .validate_domain(&self.network_id, &env::current_account_id())
            .unwrap_or_else(|e| env::panic_str(e));

        require!(invoice.amount.0 > 0, "E_ZERO_AMOUNT");

        // Vendor record comes from storage, never from the caller.
        let mut vendor = self
            .vendors
            .get(&invoice.vendor_id)
            .unwrap_or_else(|| env::panic_str("E_VENDOR_NOT_FOUND"));
        require!(vendor.active, "E_VENDOR_INACTIVE");

        // Recipient binding: mandatory even if the signature is valid.
        require!(invoice.recipient == vendor.payout_account, "E_RECIPIENT_MISMATCH");

        // Replay protection: exactly the next expected nonce.
        require!(invoice.nonce == vendor.next_nonce, "E_NONCE_MISMATCH");

        // Expiration (UTC Unix seconds).
        require!(self.now_sec() < invoice.expiration_unix_sec, "E_INVOICE_EXPIRED");

        // Ed25519 verification over the canonical payload with the
        // REGISTERED vendor key. The key never comes from agent input.
        let sig: [u8; 64] = match signature.0.try_into() {
            Ok(s) => s,
            Err(_) => env::panic_str("E_SIG_LEN"),
        };
        let message = invoice.canonical_bytes();
        let key = Self::ed25519_key_bytes(&vendor);
        require!(env::ed25519_verify(&sig, &message, &key), "E_BAD_SIGNATURE");

        // Treasury policy (max payment, daily limit incl. reservations,
        // available funds, physical balance). No mutation before this.
        let day = self.check_payment_policy(invoice.amount.0);

        // ---------------- mutations (all checks passed) ----------------

        // Consume the nonce BEFORE scheduling the transfer. Even a failed
        // transfer keeps the nonce consumed: the same invoice can never
        // trigger a second transfer.
        vendor.next_nonce = vendor
            .next_nonce
            .checked_add(1)
            .unwrap_or_else(|| env::panic_str("E_OVERFLOW"));
        self.vendors.insert(&invoice.vendor_id, &vendor);

        let payment_id = self.alloc_payment_id();
        let payment = Payment {
            id: payment_id,
            kind: PaymentKind::VendorInvoice { nonce: invoice.nonce },
            vendor_id: invoice.vendor_id.clone(),
            recipient: Some(invoice.recipient.clone()),
            amount: invoice.amount.0,
            status: PaymentStatus::Processing,
            accounting_day: day,
            created_at_sec: self.now_sec(),
            expires_at_sec: None,
        };
        self.store_payment(payment);

        self.reserve_funds(invoice.amount.0, day);

        InvoiceGuardEvent::InvoiceAccepted {
            payment_id,
            vendor_id: invoice.vendor_id.clone(),
            nonce: invoice.nonce,
            amount: invoice.amount,
            recipient: invoice.recipient.clone(),
        }
        .emit();
        InvoiceGuardEvent::PaymentProcessing {
            payment_id,
            vendor_id: invoice.vendor_id.clone(),
            amount: invoice.amount,
            recipient: invoice.recipient.clone(),
        }
        .emit();

        self.schedule_transfer(payment_id, &invoice.recipient, invoice.amount.0)
    }

    // ------------------------------------------------------------------
    // Manual payment request path
    // ------------------------------------------------------------------

    /// Submit a manual payment request for administrator approval.
    ///
    /// Stores the request fields immutably; reserves nothing; pays nothing.
    /// The contract generates the request id. Pending requests expire
    /// after `request_ttl_sec` and are lazily cleaned up when new
    /// requests arrive.
    pub fn request_manual_payment(
        &mut self,
        vendor_id: AccountId,
        amount: U128,
        memo: Option<String>,
    ) -> u64 {
        require!(self.is_agent(), "E_NOT_AGENT");
        require!(!self.paused, "E_PAUSED");
        require!(amount.0 > 0, "E_ZERO_AMOUNT");
        if let Some(m) = &memo {
            require!(m.len() <= MAX_MEMO_LEN, "E_MEMO_TOO_LONG");
        }

        // Lazy cleanup of expired pending requests. This also guarantees
        // expired entries never permanently block the queue.
        self.cleanup_expired_pending();
        require!(
            (self.pending_request_ids.len() as u64) < MAX_PENDING_REQUESTS,
            "E_QUEUE_FULL"
        );

        let payment_id = self.alloc_payment_id();
        let now = self.now_sec();
        let expires_at = now
            .checked_add(self.request_ttl_sec)
            .unwrap_or_else(|| env::panic_str("E_OVERFLOW"));

        let payment = Payment {
            id: payment_id,
            kind: PaymentKind::ManualRequest { memo },
            vendor_id: vendor_id.clone(),
            recipient: None,
            amount: amount.0,
            status: PaymentStatus::PendingApproval,
            accounting_day: self.today(),
            created_at_sec: now,
            expires_at_sec: Some(expires_at),
        };
        self.store_payment(payment);
        self.pending_request_ids.push(&payment_id);

        InvoiceGuardEvent::ManualPaymentRequested {
            payment_id,
            vendor_id,
            amount,
        }
        .emit();

        payment_id
    }

    /// Approve a pending manual payment request.
    ///
    /// Callable only by the administrator. Executes *exactly* the stored
    /// request fields — the only input is the request id, so neither the
    /// administrator nor the agent can change amount, vendor, or memo
    /// during approval. The recipient is always bound to the vendor's
    /// registered payout account at approval time.
    ///
    /// Revalidates current vendor status and all payment policy before
    /// reserving funds. If validation fails the request stays pending
    /// (documented rule) unless it is expired, in which case it fails
    /// outright.
    pub fn approve_manual_payment(&mut self, request_id: u64) {
        require!(self.is_admin(), "E_NOT_ADMIN");

        let mut payment = self
            .payments
            .get(&request_id)
            .unwrap_or_else(|| env::panic_str("E_PAYMENT_NOT_FOUND"));
        require!(payment.status == PaymentStatus::PendingApproval, "E_NOT_PENDING");

        let now = self.now_sec();
        match payment.expires_at_sec {
            Some(exp) => require!(now < exp, "E_REQUEST_EXPIRED"),
            None => env::panic_str("E_NO_EXPIRY"),
        }

        // Revalidate vendor status at approval time. If the vendor is
        // unknown or inactive, this call fails and the request stays
        // pending; the admin may register/activate the vendor first and
        // approve again.
        let vendor = self
            .vendors
            .get(&payment.vendor_id)
            .unwrap_or_else(|| env::panic_str("E_VENDOR_NOT_FOUND"));
        require!(vendor.active, "E_VENDOR_INACTIVE");

        // Revalidate current payment policy (max payment, daily limit,
        // available funds, physical balance).
        let day = self.check_payment_policy(payment.amount);

        // Bind the recipient to the registered payout account.
        let recipient = vendor.payout_account.clone();

        // ---------------- mutations ----------------
        payment.status = PaymentStatus::Processing;
        payment.recipient = Some(recipient.clone());
        // Daily accounting is booked to the approval day (the day funds
        // are actually reserved).
        payment.accounting_day = day;
        // The expiry governed only the approval window.
        payment.expires_at_sec = None;
        self.payments.insert(&request_id, &payment);
        self.remove_pending_id(request_id);

        self.reserve_funds(payment.amount, day);

        InvoiceGuardEvent::ManualPaymentApproved {
            payment_id: request_id,
            vendor_id: payment.vendor_id.clone(),
            amount: U128(payment.amount),
            recipient: recipient.clone(),
        }
        .emit();
        InvoiceGuardEvent::PaymentProcessing {
            payment_id: request_id,
            vendor_id: payment.vendor_id.clone(),
            amount: U128(payment.amount),
            recipient: recipient.clone(),
        }
        .emit();

        let _ = self.schedule_transfer(request_id, &recipient, payment.amount);
    }

    /// Reject a pending manual payment request.
    ///
    /// Callable only by the administrator. Transitions the request to
    /// REJECTED regardless of expiry (rejecting an already-expired request
    /// is an explicit way to discard it).
    pub fn reject_manual_payment(&mut self, request_id: u64) {
        require!(self.is_admin(), "E_NOT_ADMIN");

        let mut payment = self
            .payments
            .get(&request_id)
            .unwrap_or_else(|| env::panic_str("E_PAYMENT_NOT_FOUND"));
        require!(payment.status == PaymentStatus::PendingApproval, "E_NOT_PENDING");

        payment.status = PaymentStatus::Rejected;
        payment.expires_at_sec = None;
        self.payments.insert(&request_id, &payment);
        self.remove_pending_id(request_id);

        InvoiceGuardEvent::ManualPaymentRejected { payment_id: request_id }.emit();
    }

    // ------------------------------------------------------------------
    // Private transfer-finalization callback
    // ------------------------------------------------------------------

    /// `#[private]`: only this contract can call it (the macro panics if
    /// `predecessor != current_account_id` and rejects attached deposits).
    ///
    /// Finalizes a PROCESSING payment exactly once:
    /// - verifies exactly one promise result is available,
    /// - verifies the payment exists and is still PROCESSING
    ///   (this is what makes double finalization impossible),
    /// - applies ledger/daily accounting against the payment's original
    ///   UTC day,
    /// - transitions to PAID or FAILED.
    ///
    /// A result that is not a clean success/failure (oversized payload)
    /// fails closed: accounting is left untouched and the payment stays
    /// PROCESSING rather than guessing.
    #[private]
    pub fn on_transfer_complete(&mut self, payment_id: u64) {
        require!(env::promise_results_count() == 1, "E_CALLBACK_COUNT");

        let success = match env::promise_result_checked(0, MAX_PROMISE_RESULT_LEN) {
            Ok(_) => true,
            Err(PromiseError::Failed) => false,
            Err(_) => env::panic_str("E_CALLBACK_RESULT"),
        };

        let mut payment = self
            .payments
            .get(&payment_id)
            .unwrap_or_else(|| env::panic_str("E_PAYMENT_NOT_FOUND"));
        // Duplicate or out-of-band callbacks cannot finalize twice.
        require!(payment.status == PaymentStatus::Processing, "E_NOT_PROCESSING");

        self.apply_transfer_outcome(&payment, success);

        payment.status = if success { PaymentStatus::Paid } else { PaymentStatus::Failed };
        self.payments.insert(&payment_id, &payment);
        self.trim_payment_history();

        if success {
            InvoiceGuardEvent::PaymentPaid {
                payment_id,
                vendor_id: payment.vendor_id.clone(),
                amount: U128(payment.amount),
            }
            .emit();
        } else {
            InvoiceGuardEvent::PaymentFailed {
                payment_id,
                vendor_id: payment.vendor_id.clone(),
                amount: U128(payment.amount),
            }
            .emit();
        }
    }

    // ------------------------------------------------------------------
    // Shared internals
    // ------------------------------------------------------------------

    /// Schedules the NEAR transfer to `recipient` followed by the private
    /// callback with a reserved gas allocation.
    pub(crate) fn schedule_transfer(
        &mut self,
        payment_id: u64,
        recipient: &AccountId,
        amount: u128,
    ) -> Promise {
        Promise::new(recipient.clone())
            .transfer(Self::near_token(amount))
            .then(
                Self::ext_self()
                    .with_static_gas(CALLBACK_GAS)
                    .on_transfer_complete(payment_id),
            )
    }

    /// Allocates the next sequential payment id. Contract-generated only.
    pub(crate) fn alloc_payment_id(&mut self) -> u64 {
        let id = self.next_payment_id;
        self.next_payment_id = id
            .checked_add(1)
            .unwrap_or_else(|| env::panic_str("E_OVERFLOW"));
        id
    }

    /// Stores a payment and trims terminal history beyond the retention
    /// window. Active payments are never deleted.
    pub(crate) fn store_payment(&mut self, payment: Payment) {
        self.payments.insert(&payment.id, &payment);
        self.trim_payment_history();
    }

    /// Deletes the oldest *terminal* payment records while the total
    /// record count exceeds the retention window.
    ///
    /// Payments are deleted oldest-first. Because ids are sequential and
    /// deletion only happens at the low end, ids between
    /// `oldest_retained_payment_id` and `next_payment_id` map to live
    /// records (this is what `get_recent_payments` relies on).
    ///
    /// Replay protection is NOT affected: vendor nonces live in the
    /// vendor records, not in payment history.
    ///
    /// A non-terminal oldest record (e.g. a stuck PROCESSING payment)
    /// blocks deletion until it resolves; the window may temporarily
    /// exceed the cap. This is a documented limitation.
    pub(crate) fn trim_payment_history(&mut self) {
        while self.payments.len() > MAX_RETAINED_PAYMENTS {
            let oldest = self.oldest_retained_payment_id;
            if oldest >= self.next_payment_id {
                break;
            }
            match self.payments.get(&oldest) {
                Some(p) if p.status.is_terminal() => {
                    self.payments.remove(&oldest);
                    self.oldest_retained_payment_id = oldest.saturating_add(1);
                }
                Some(_) => break, // oldest is still active; retry later
                None => {
                    // Defensive: pointer past a missing record.
                    self.oldest_retained_payment_id = oldest.saturating_add(1);
                }
            }
        }
    }

    /// Removes a request id from the pending queue (swap-remove).
    /// Queue order is not semantically meaningful; membership is.
    pub(crate) fn remove_pending_id(&mut self, request_id: u64) {
        let len = self.pending_request_ids.len();
        for i in 0..len {
            if self.pending_request_ids.get(i) == Some(request_id) {
                // Swap-remove: O(1).
                self.pending_request_ids.swap_remove(i);
                return;
            }
        }
    }

    /// Marks expired pending requests as EXPIRED and frees their queue
    /// slots. Called before enforcing the pending-queue limit so expired
    /// entries never permanently block new requests.
    pub(crate) fn cleanup_expired_pending(&mut self) {
        let now = self.now_sec();
        let ids: Vec<u64> = self.pending_request_ids.to_vec();
        for id in ids {
            match self.payments.get(&id) {
                Some(mut p) => {
                    if p.status == PaymentStatus::PendingApproval
                        && p.expires_at_sec.map_or(false, |exp| exp <= now)
                    {
                        p.status = PaymentStatus::Expired;
                        p.expires_at_sec = None;
                        self.payments.insert(&id, &p);
                        self.remove_pending_id(id);
                        InvoiceGuardEvent::ManualPaymentExpired { payment_id: id }.emit();
                    }
                }
                None => {
                    // Dangling id; drop it from the queue.
                    self.remove_pending_id(id);
                }
            }
        }
    }

    /// Extracts the raw 32-byte Ed25519 key from a registered vendor key.
    pub(crate) fn ed25519_key_bytes(vendor: &crate::types::Vendor) -> [u8; 32] {
        Self::ed25519_bytes(&vendor.signing_key)
    }
}
