//! Read-only views for dashboards, clients, and tests.
//!
//! Views never mutate state, so they cannot roll the daily window or
//! lazily clean up anything. Where relevant this is documented on the
//! specific view.

use near_sdk::{near_bindgen, AccountId};

use crate::config::*;
use crate::types::*;
use crate::{Contract, ContractExt};

#[near_bindgen]
impl Contract {
    /// Contract configuration and current policy.
    pub fn get_config(&self) -> ConfigView {
        ConfigView {
            admin: self.admin.clone(),
            agent: self.agent.clone(),
            network_id: self.network_id.clone(),
            paused: self.paused,
            max_payment: self.max_payment.into(),
            daily_limit: self.daily_limit.into(),
            reserve_floor: self.reserve_floor.into(),
            request_ttl_sec: self.request_ttl_sec,
            next_payment_id: self.next_payment_id,
            pending_requests: self.pending_request_ids.len(),
            vendors: self.vendor_ids.len(),
        }
    }

    /// The signature domain a vendor must sign over: protocol id,
    /// protocol version, network id, and this contract's account.
    /// Client tooling builds the canonical payload from exactly these
    /// values.
    pub fn get_invoice_domain(&self) -> InvoiceDomainView {
        InvoiceDomainView {
            protocol_id: crate::config::PROTOCOL_ID.to_string(),
            protocol_version: crate::config::PROTOCOL_VERSION,
            network_id: self.network_id.clone(),
            contract_id: near_sdk::env::current_account_id(),
        }
    }

    /// A single vendor record, or `null` if not registered.
    pub fn get_vendor(&self, vendor_id: AccountId) -> Option<VendorView> {
        self.vendors.get(&vendor_id).map(|v| VendorView {
            vendor_id,
            payout_account: v.payout_account,
            signing_key: v.signing_key,
            active: v.active,
            next_nonce: v.next_nonce,
        })
    }

    /// All registered vendors (bounded by `MAX_VENDORS`).
    pub fn get_vendors(&self) -> Vec<VendorView> {
        self.vendor_ids
            .iter()
            .filter_map(|vendor_id| self.vendors.get(&vendor_id).map(|v| VendorView {
                vendor_id,
                payout_account: v.payout_account,
                signing_key: v.signing_key,
                active: v.active,
                next_nonce: v.next_nonce,
            }))
            .collect()
    }

    /// Treasury state. `today_*` figures refer to `current_day`, i.e. the
    /// UTC day of the last treasury operation (views cannot roll the
    /// day). `available` is the spendable amount under current policy.
    pub fn get_treasury(&self) -> TreasuryView {
        let today = self.daily.get(&self.current_day).unwrap_or_default();
        TreasuryView {
            ledger_balance: self.ledger_balance.into(),
            total_reserved: self.total_reserved.into(),
            available: self.available_funds().into(),
            reserve_floor: self.reserve_floor.into(),
            current_day: self.current_day,
            today_spent: today.spent.into(),
            today_reserved: today.reserved.into(),
        }
    }

    /// A single payment record, or `null` if unknown or trimmed from the
    /// retention window. Replay protection never depends on this
    /// (nonces live in vendor records).
    pub fn get_payment(&self, payment_id: u64) -> Option<PaymentView> {
        self.payments.get(&payment_id).map(|p| p.into())
    }

    /// Most recent payment records, newest first.
    ///
    /// `limit` defaults to 25 and is capped at 100. Records beyond the
    /// retention window are absent; ids between the oldest retained id
    /// and the next id are contiguous.
    pub fn get_recent_payments(&self, limit: Option<u64>) -> Vec<PaymentView> {
        let limit = limit.unwrap_or(25).min(MAX_QUERY_LIMIT) as usize;
        let mut out = Vec::with_capacity(limit.min(32));
        let mut id = self.next_payment_id;
        while id > self.oldest_retained_payment_id && out.len() < limit {
            id -= 1;
            if let Some(p) = self.payments.get(&id) {
                out.push(p.into());
            }
        }
        out
    }

    /// Pending manual payment requests. Because views cannot perform the
    /// lazy expiry cleanup, entries whose deadline has passed may still
    /// appear here with status `pending_approval`; check
    /// `expires_at_sec` against the current time client-side, or call
    /// `request_manual_payment` / `approve_manual_payment` to trigger
    /// the cleanup.
    pub fn get_pending_requests(&self) -> Vec<PaymentView> {
        self.pending_request_ids
            .iter()
            .filter_map(|id| self.payments.get(&id).map(|p| p.into()))
            .collect()
    }
}
