//! Administrator-only configuration and the vendor registry.
//!
//! Every method here is gated by `require!(self.is_admin(), ...)` — the
//! caller must be the account configured at init. There is deliberately
//! **no** admin method that withdraws funds and **no** admin method that
//! can alter a payment's stored fields: the administrator can only
//! change policy (limits, floors, TTL, pause), manage the vendor
//! registry, and approve or reject *stored* manual requests.

use near_sdk::json_types::U128;
use near_sdk::{env, near_bindgen, require, AccountId, PublicKey};

use crate::config::*;
use crate::events::InvoiceGuardEvent;
use crate::types::*;
use crate::{Contract, ContractExt};

#[near_bindgen]
impl Contract {
    // ------------------------------------------------------------------
    // Policy configuration (admin only)
    // ------------------------------------------------------------------

    /// Change the authorized agent account.
    ///
    /// Takes effect immediately for future calls; in-flight payments are
    /// unaffected. The agent may never be the admin itself (a distinct
    /// human approval point must always exist).
    pub fn set_agent(&mut self, agent: AccountId) {
        require!(self.is_admin(), "E_NOT_ADMIN");
        require!(agent != self.admin, "E_ADMIN_IS_AGENT");
        self.agent = agent.clone();
        InvoiceGuardEvent::AgentChanged { agent }.emit();
    }

    /// Change the per-payment maximum (yoctoNEAR).
    pub fn set_max_payment(&mut self, max_payment: U128) {
        require!(self.is_admin(), "E_NOT_ADMIN");
        require!(max_payment.0 > 0, "E_ZERO_MAX_PAYMENT");
        self.max_payment = max_payment.0;
        InvoiceGuardEvent::PaymentLimitChanged { max_payment }.emit();
    }

    /// Change the daily spending limit (yoctoNEAR, per UTC day,
    /// spent + reserved).
    pub fn set_daily_limit(&mut self, daily_limit: U128) {
        require!(self.is_admin(), "E_NOT_ADMIN");
        require!(daily_limit.0 > 0, "E_ZERO_DAILY_LIMIT");
        self.daily_limit = daily_limit.0;
        InvoiceGuardEvent::DailyLimitChanged { daily_limit }.emit();
    }

    /// Change the minimum retained ledger balance (yoctoNEAR).
    ///
    /// Lowering the floor frees funds for payments; raising it can only
    /// reduce future availability (never triggers any refund).
    pub fn set_reserve_floor(&mut self, reserve_floor: U128) {
        require!(self.is_admin(), "E_NOT_ADMIN");
        self.reserve_floor = reserve_floor.0;
        InvoiceGuardEvent::ReserveFloorChanged { reserve_floor }.emit();
    }

    /// Change the manual-request approval window (seconds).
    ///
    /// `0` makes requests expire immediately (useful for tests and for
    /// locking the queue down); the upper bound is 30 days.
    pub fn set_request_ttl_seconds(&mut self, ttl_seconds: u64) {
        require!(self.is_admin(), "E_NOT_ADMIN");
        require!(ttl_seconds <= MAX_REQUEST_TTL_SEC, "E_TTL_TOO_LONG");
        self.request_ttl_sec = ttl_seconds;
        InvoiceGuardEvent::RequestTtlChanged { ttl_seconds }.emit();
    }

    /// Pause both payment paths. Admin only. View calls still work.
    pub fn pause(&mut self) {
        require!(self.is_admin(), "E_NOT_ADMIN");
        require!(!self.paused, "E_ALREADY_PAUSED");
        self.paused = true;
        InvoiceGuardEvent::ContractPaused {}.emit();
    }

    /// Unpause both payment paths. Admin only.
    pub fn unpause(&mut self) {
        require!(self.is_admin(), "E_NOT_ADMIN");
        require!(self.paused, "E_NOT_PAUSED");
        self.paused = false;
        InvoiceGuardEvent::ContractUnpaused {}.emit();
    }

    // ------------------------------------------------------------------
    // Vendor registry (admin only)
    // ------------------------------------------------------------------

    /// Register a new vendor with an Ed25519 signing key and a payout
    /// account.
    ///
    /// - The signing key is the *only* key whose signatures this contract
    ///   will ever accept for this vendor; it comes from the admin, never
    ///   from the agent.
    /// - `payout_account` is the only account vendor payments may be sent
    ///   to. It must not be the contract itself (a transfer to the
    ///   contract would bypass ledger accounting and strand funds).
    /// - Replay protection starts at nonce 1 (`next_nonce = 1`).
    ///
    /// Re-registering an existing vendor is refused; use
    /// [`Contract::rotate_vendor_key`],
    /// [`Contract::set_vendor_payout`], and
    /// [`Contract::set_vendor_active`] to evolve a vendor record.
    pub fn register_vendor(
        &mut self,
        vendor_id: AccountId,
        signing_key: PublicKey,
        payout_account: AccountId,
    ) {
        require!(self.is_admin(), "E_NOT_ADMIN");
        require!(
            self.vendors.get(&vendor_id).is_none(),
            "E_VENDOR_EXISTS"
        );
        require!(
            (self.vendor_ids.len() as u64) < MAX_VENDORS,
            "E_TOO_MANY_VENDORS"
        );
        // Fail fast on a non-Ed25519 key (also enforced again at
        // verification time).
        let _ = Self::ed25519_bytes(&signing_key);
        require!(
            payout_account != env::current_account_id(),
            "E_PAYOUT_IS_CONTRACT"
        );

        let vendor = Vendor {
            signing_key,
            payout_account,
            active: true,
            // Nonce 0 is never valid (next_nonce starts at 1).
            next_nonce: 1,
        };
        self.vendors.insert(&vendor_id, &vendor);
        self.vendor_ids.push(&vendor_id);

        InvoiceGuardEvent::VendorRegistered {
            vendor_id,
            payout_account: vendor.payout_account,
        }
        .emit();
    }

    /// Change a vendor's payout account. Affects future payments only;
    /// in-flight payments keep their bound recipient.
    pub fn set_vendor_payout(&mut self, vendor_id: AccountId, payout_account: AccountId) {
        require!(self.is_admin(), "E_NOT_ADMIN");
        let mut vendor = self
            .vendors
            .get(&vendor_id)
            .unwrap_or_else(|| env::panic_str("E_VENDOR_NOT_FOUND"));
        require!(
            payout_account != env::current_account_id(),
            "E_PAYOUT_IS_CONTRACT"
        );
        vendor.payout_account = payout_account;
        self.vendors.insert(&vendor_id, &vendor);

        InvoiceGuardEvent::VendorPayoutChanged {
            vendor_id,
            payout_account: vendor.payout_account,
        }
        .emit();
    }

    /// Rotate a vendor's Ed25519 signing key.
    ///
    /// Signatures from the old key become invalid immediately. Replay
    /// protection is *not* reset: the nonce sequence continues, so old
    /// invoices stay spent even after rotation.
    pub fn rotate_vendor_key(&mut self, vendor_id: AccountId, signing_key: PublicKey) {
        require!(self.is_admin(), "E_NOT_ADMIN");
        let mut vendor = self
            .vendors
            .get(&vendor_id)
            .unwrap_or_else(|| env::panic_str("E_VENDOR_NOT_FOUND"));
        let _ = Self::ed25519_bytes(&signing_key);
        vendor.signing_key = signing_key;
        self.vendors.insert(&vendor_id, &vendor);

        InvoiceGuardEvent::VendorKeyRotated { vendor_id }.emit();
    }

    /// Activate or deactivate a vendor. Inactive vendors' invoices and
    /// manual requests are refused (existing pending approvals can still
    /// be rejected, but no longer approved).
    pub fn set_vendor_active(&mut self, vendor_id: AccountId, active: bool) {
        require!(self.is_admin(), "E_NOT_ADMIN");
        let mut vendor = self
            .vendors
            .get(&vendor_id)
            .unwrap_or_else(|| env::panic_str("E_VENDOR_NOT_FOUND"));
        vendor.active = active;
        self.vendors.insert(&vendor_id, &vendor);

        if active {
            InvoiceGuardEvent::VendorActivated { vendor_id }.emit();
        } else {
            InvoiceGuardEvent::VendorDeactivated { vendor_id }.emit();
        }
    }

    // ------------------------------------------------------------------
    // Shared key helper
    // ------------------------------------------------------------------

    /// Extracts the raw 32-byte Ed25519 key from a `PublicKey`,
    /// rejecting non-Ed25519 keys.
    pub(crate) fn ed25519_bytes(key: &PublicKey) -> [u8; 32] {
        use near_sdk::PublicKeyHandle;
        match PublicKeyHandle::from(key) {
            PublicKeyHandle::ED25519(data) => data,
            _ => env::panic_str("E_KEY_NOT_ED25519"),
        }
    }
}
