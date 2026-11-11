//! Canonical vendor-signed invoice payload.
//!
//! This is one of the most security-critical components of InvoiceGuard.
//!
//! # Signed payload format
//!
//! The message is the deterministic [Borsh](https://borsh.io) serialization
//! of [`SignedInvoice`] with the following **fixed field order**:
//!
//! | # | field                | type      | encoding                        |
//! |---|----------------------|-----------|---------------------------------|
//! | 1 | `protocol_id`        | `String`  | `u32` LE length + UTF-8 bytes   |
//! | 2 | `protocol_version`   | `u32`     | 4 bytes LE                      |
//! | 3 | `network_id`         | `String`  | `u32` LE length + UTF-8 bytes   |
//! | 4 | `contract_id`        | `AccountId` | `u32` LE length + UTF-8 bytes |
//! | 5 | `vendor_id`          | `AccountId` | `u32` LE length + UTF-8 bytes |
//! | 6 | `nonce`              | `u64`     | 8 bytes LE                      |
//! | 7 | `recipient`          | `AccountId` | `u32` LE length + UTF-8 bytes |
//! | 8 | `amount`             | `u128`    | 16 bytes LE                     |
//! | 9 | `expiration_unix_sec`| `u64`     | 8 bytes LE                      |
//!
//! `AccountId` serializes in Borsh exactly like a `String`
//! (`u32` LE length prefix + UTF-8 bytes), which is the `near-account-id`
//! crate's documented Borsh layout.
//!
//! Domain separation is explicit: protocol id, protocol version, network
//! identifier, and contract account are all signed fields. A signature
//! produced for another protocol, another NEAR network, or another
//! InvoiceGuard contract can never be replayed here.
//!
//! No memo is included in the signed payload: an invoice memo has no
//! authorization significance in this protocol, and we do not pretend
//! otherwise. (Manual requests carry a memo, but those are authorized by
//! administrator approval of the stored request, not by a signature.)

use near_sdk::borsh;
use near_sdk::json_types::U128;
use near_sdk::{near, AccountId};

use crate::config::{PROTOCOL_ID, PROTOCOL_VERSION};

/// The vendor-signed invoice. All fields are covered by the signature.
///
/// The same struct is used for JSON method arguments (`pay_invoice`),
/// so the client never re-types fields: what the contract verifies is
/// exactly what the client sent.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedInvoice {
    /// Must equal `"invoiceguard"`.
    pub protocol_id: String,
    /// Must equal `1`.
    pub protocol_version: u32,
    /// Must equal the contract's configured network identifier
    /// (e.g. `"sandbox"` or `"testnet"`).
    pub network_id: String,
    /// Must equal this contract's own account id.
    pub contract_id: AccountId,
    /// The vendor that signed the invoice. Selects the vendor record.
    pub vendor_id: AccountId,
    /// Per-vendor monotonic invoice nonce. Must equal the vendor's
    /// current `next_nonce` at payment time.
    pub nonce: u64,
    /// Payment recipient. Must equal the vendor's registered payout
    /// account; this is checked independently of the signature.
    pub recipient: AccountId,
    /// Payment amount in yoctoNEAR.
    pub amount: U128,
    /// Expiration time as Unix seconds (UTC). The payment must be
    /// executed strictly before this time.
    pub expiration_unix_sec: u64,
}

impl SignedInvoice {
    /// Validates the domain-separation fields against this deployment.
    ///
    /// These checks are performed *in addition to* Ed25519 verification:
    /// a signature could be perfectly valid yet be intended for a
    /// different protocol, network, or contract instance.
    pub fn validate_domain(
        &self,
        expected_network_id: &str,
        expected_contract_id: &AccountId,
    ) -> Result<(), &'static str> {
        if self.protocol_id != PROTOCOL_ID {
            return Err("E_DOMAIN_PROTOCOL");
        }
        if self.protocol_version != PROTOCOL_VERSION {
            return Err("E_DOMAIN_VERSION");
        }
        if self.network_id != expected_network_id {
            return Err("E_DOMAIN_NETWORK");
        }
        if &self.contract_id != expected_contract_id {
            return Err("E_DOMAIN_CONTRACT");
        }
        Ok(())
    }

    /// Deterministic Borsh serialization of the signed payload.
    ///
    /// This is the exact byte string the vendor signs and the contract
    /// verifies. It cannot fail for an in-memory struct
    /// (the writer is a `Vec<u8>`).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        borsh::to_vec(self)
            .unwrap_or_else(|_| near_sdk::env::panic_str("E_INVOICE_SERIALIZE"))
    }
}
