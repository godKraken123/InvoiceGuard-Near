//! Shared data types and storage key allocation.
//!
//! Internal records ([`Vendor`], [`Payment`], [`DailyEntry`]) are stored as
//! Borsh state. The `*View` structs are JSON-only and are what clients see;
//! every financial value is exposed as [`U128`] so it crosses the JSON
//! boundary as a decimal string and never loses precision in JavaScript.

use near_sdk::{near, AccountId, PublicKey};

pub use near_sdk::json_types::U128;

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

/// A registered vendor.
///
/// The signing key is the *only* key accepted for this vendor's invoices and
/// the payout account is the *only* account its funds may be sent to. Both are
/// administrator-controlled; neither ever comes from agent input.
#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct Vendor {
    pub signing_key: PublicKey,
    pub payout_account: AccountId,
    pub active: bool,
    /// Next invoice nonce that will be accepted. Starts at `1`, so nonce `0`
    /// is never valid.
    pub next_nonce: u64,
}

/// Per-UTC-day accounting entry.
///
/// `spent + reserved` counts against the daily limit. `reserved` holds funds
/// for in-flight transfers and is released when the transfer finalizes.
#[near(serializers = [borsh])]
#[derive(Clone, Copy, Debug, Default)]
pub struct DailyEntry {
    pub spent: u128,
    pub reserved: u128,
}

/// How a payment was authorized. The two paths are deliberately distinct:
/// a vendor signature is never treated as administrator approval, and vice
/// versa.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PaymentKind {
    /// Authorized by a vendor-signed invoice carrying this nonce.
    VendorInvoice { nonce: u64 },
    /// Authorized by administrator approval of a stored manual request.
    ManualRequest { memo: Option<String> },
}

/// Payment state machine.
///
/// `PendingApproval` exists only for manual requests; the vendor-invoice path
/// starts at `Processing`. `Processing` is finalized exactly once by the
/// private transfer callback.
#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    PendingApproval,
    Processing,
    Paid,
    Failed,
    Rejected,
    Expired,
}

impl PaymentStatus {
    /// True once the payment can never change again. Only terminal records
    /// may be trimmed from the bounded history window.
    pub fn is_terminal(&self) -> bool {
        !matches!(
            self,
            PaymentStatus::PendingApproval | PaymentStatus::Processing
        )
    }
}

/// Internal payment / manual-request record.
///
/// Manual requests reuse the same record shape (kind `ManualRequest`, status
/// `PendingApproval`) so there is a single id space and a single finalization
/// path. `recipient` is `None` until a manual request is approved; at approval
/// it is bound to the vendor's registered payout account.
#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct Payment {
    pub id: u64,
    pub kind: PaymentKind,
    pub vendor_id: AccountId,
    pub recipient: Option<AccountId>,
    pub amount: u128,
    pub status: PaymentStatus,
    /// UTC day (Unix seconds / 86_400) the reservation is booked to.
    pub accounting_day: u64,
    /// Creation time as Unix seconds (UTC).
    pub created_at_sec: u64,
    /// Manual-request approval deadline as Unix seconds (UTC). `None` once
    /// approved/rejected, or for invoice payments.
    pub expires_at_sec: Option<u64>,
}

// ---------------------------------------------------------------------------
// JSON views
// ---------------------------------------------------------------------------

#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct VendorView {
    pub vendor_id: AccountId,
    pub payout_account: AccountId,
    pub signing_key: PublicKey,
    pub active: bool,
    pub next_nonce: u64,
}

#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct PaymentView {
    pub id: u64,
    pub kind: PaymentKind,
    pub vendor_id: AccountId,
    pub recipient: Option<AccountId>,
    pub amount: U128,
    pub status: PaymentStatus,
    pub accounting_day: u64,
    pub created_at_sec: u64,
    pub expires_at_sec: Option<u64>,
}

impl From<Payment> for PaymentView {
    fn from(p: Payment) -> Self {
        Self {
            id: p.id,
            kind: p.kind,
            vendor_id: p.vendor_id,
            recipient: p.recipient,
            amount: U128(p.amount),
            status: p.status,
            accounting_day: p.accounting_day,
            created_at_sec: p.created_at_sec,
            expires_at_sec: p.expires_at_sec,
        }
    }
}

impl From<&Payment> for PaymentView {
    fn from(p: &Payment) -> Self {
        p.clone().into()
    }
}

#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct ConfigView {
    pub admin: AccountId,
    pub agent: AccountId,
    pub network_id: String,
    pub paused: bool,
    pub max_payment: U128,
    pub daily_limit: U128,
    pub reserve_floor: U128,
    pub request_ttl_sec: u64,
    pub next_payment_id: u64,
    pub pending_requests: u64,
    pub vendors: u64,
}

#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct TreasuryView {
    pub ledger_balance: U128,
    pub total_reserved: U128,
    pub available: U128,
    pub reserve_floor: U128,
    pub current_day: u64,
    pub today_spent: U128,
    pub today_reserved: U128,
}

/// The exact domain a vendor signs over. Clients fetch this and build the
/// canonical payload from these values; see `contract/src/invoice.rs`.
#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct InvoiceDomainView {
    pub protocol_id: String,
    pub protocol_version: u32,
    pub network_id: String,
    pub contract_id: AccountId,
}

// ---------------------------------------------------------------------------
// Storage prefixes
// ---------------------------------------------------------------------------

/// Unique Borsh prefix for each persistent collection. Adding a variant is
/// safe; reordering or removing one would reinterpret existing state.
#[near(serializers = [borsh])]
#[derive(near_sdk::BorshStorageKey)]
pub enum StorageKey {
    Vendors,
    VendorIds,
    Daily,
    Payments,
    PendingRequestIds,
}
