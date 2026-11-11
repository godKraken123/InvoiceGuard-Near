//! InvoiceGuard: an on-chain payment authorization layer for AI agents.
//!
//! The agent may *request* payments, but only this contract decides whether a
//! payment is authorized. Vendor-signed invoices and administrator-approved
//! manual requests are separate paths and are never conflated.
//!
//! Module map:
//! - [`config`]  compile-time policy constants and storage key layout
//! - [`types`]   persisted records, JSON views, storage prefixes
//! - [`invoice`] canonical Borsh payload signed by vendors
//! - [`admin`]   administrator policy + vendor registry
//! - [`treasury`] ledger, reservations, daily accounting, funding
//! - [`payments`] invoice/manual payment paths + private callback
//! - [`views`]   read-only views
//! - [`events`]  NEP-297 events

use near_sdk::collections::{LookupMap, UnorderedMap, Vector};
use near_sdk::json_types::U128;
use near_sdk::{env, ext_contract, near, near_bindgen, require, AccountId, PanicOnDefault};

use crate::config::*;
use crate::types::*;

pub mod admin;
pub mod config;
pub mod events;
pub mod invoice;
pub mod payments;
pub mod treasury;
pub mod types;
pub mod views;

#[cfg(test)]
mod tests;

/// Private self-callback interface. `#[near_bindgen]` also generates
/// `Contract::ext_self()`, which is what [`payments`] uses to schedule the
/// callback; this trait documents the callback ABI explicitly.
#[ext_contract(ext_self)]
pub trait ExtSelf {
    fn on_transfer_complete(&mut self, payment_id: u64);
}

/// Contract state.
///
/// Field names are load-bearing: they are referenced directly by the treasury,
/// payment, admin, and view modules and by the unit tests.
#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Contract {
    // ---- authorization -------------------------------------------------
    admin: AccountId,
    agent: AccountId,
    /// Immutable chain-domain identifier (e.g. `sandbox`, `testnet`).
    network_id: String,
    paused: bool,

    // ---- treasury policy ----------------------------------------------
    /// Internal ledger. Only explicit funding increases this value.
    ledger_balance: u128,
    /// Sum of amounts reserved for in-flight transfers.
    total_reserved: u128,
    /// Funds that must remain available at all times.
    reserve_floor: u128,
    /// Maximum amount of a single payment.
    max_payment: u128,
    /// Maximum `spent + reserved` per UTC day.
    daily_limit: u128,

    // ---- daily accounting ---------------------------------------------
    daily: LookupMap<u64, DailyEntry>,
    /// Oldest UTC day that may still have an accounting entry.
    oldest_day: u64,
    /// UTC day of the last treasury operation (views cannot roll it).
    current_day: u64,

    // ---- vendor registry ----------------------------------------------
    vendors: LookupMap<AccountId, Vendor>,
    vendor_ids: Vector<AccountId>,

    // ---- payments (also holds manual requests) ------------------------
    payments: UnorderedMap<u64, Payment>,
    next_payment_id: u64,
    /// Oldest payment id that may still be retained.
    oldest_retained_payment_id: u64,

    // ---- manual request queue -----------------------------------------
    pending_request_ids: Vector<u64>,
    /// Approval window for newly created manual requests, in seconds.
    request_ttl_sec: u64,
}

#[near_bindgen]
impl Contract {
    /// Initialize the contract.
    ///
    /// `network_id` is part of the signed invoice domain and must match the
    /// deployment. `max_payment` and `daily_limit` are yoctoNEAR. The
    /// operations headroom is a protocol constant, not admin-configurable.
    #[init]
    pub fn new(
        admin: AccountId,
        agent: AccountId,
        network_id: String,
        max_payment: U128,
        daily_limit: U128,
        reserve_floor: U128,
    ) -> Self {
        require!(admin != agent, "E_ADMIN_IS_AGENT");
        require!(!network_id.is_empty(), "E_EMPTY_NETWORK_ID");
        require!(
            network_id.len() <= MAX_NETWORK_ID_LEN,
            "E_NETWORK_ID_TOO_LONG"
        );
        require!(max_payment.0 > 0, "E_ZERO_MAX_PAYMENT");
        require!(daily_limit.0 > 0, "E_ZERO_DAILY_LIMIT");

        let today = env::block_timestamp() / NANOS_PER_SEC / SECONDS_PER_DAY;

        Self {
            admin,
            agent,
            network_id,
            paused: false,
            ledger_balance: 0,
            total_reserved: 0,
            reserve_floor: reserve_floor.0,
            max_payment: max_payment.0,
            daily_limit: daily_limit.0,
            daily: LookupMap::new(StorageKey::Daily),
            oldest_day: today,
            current_day: today,
            vendors: LookupMap::new(StorageKey::Vendors),
            vendor_ids: Vector::new(StorageKey::VendorIds),
            payments: UnorderedMap::new(StorageKey::Payments),
            next_payment_id: 0,
            oldest_retained_payment_id: 0,
            pending_request_ids: Vector::new(StorageKey::PendingRequestIds),
            request_ttl_sec: DEFAULT_REQUEST_TTL_SEC,
        }
    }
}
