//! Treasury accounting: internal ledger, per-day spending limits,
//! reservations, and fund finalization.
//!
//! Invariants maintained by this module:
//!
//! 1. `total_reserved` equals the sum of `reserved` across all days and is
//!    only ever increased by [`Contract::reserve_funds`] and decreased by
//!    [`Contract::apply_transfer_outcome`].
//! 2. Reservations are only created after *all* policy checks passed
//!    (pause, vendor, signature, nonce, expiry, max payment, daily limit,
//!    available funds).
//! 3. `available >= amount` is checked before reserving, where
//!    `available = ledger - total_reserved - reserve_floor - headroom`
//!    (saturating). The headroom keeps the contract's account able to pay
//!    for storage growth.
//! 4. A reservation remembers its original UTC day
//!    ([`Payment::accounting_day`]); finalization after midnight still
//!    books against the original day.
//! 5. Every arithmetic operation on treasury values is checked and fails
//!    closed (`require!`) on overflow/underflow.
//! 6. Daily entries for past days are pruned only when their `reserved`
//!    amount is zero, so a reservation can always be finalized.

use near_sdk::{env, require, NearToken};

use crate::config::*;
use crate::types::*;
use crate::{Contract, ContractExt};

#[near_sdk::near_bindgen]
impl Contract {
    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Current block time as Unix seconds (UTC).
    /// `env::block_timestamp()` is nanoseconds; we normalize once, here.
    pub(crate) fn now_sec(&self) -> u64 {
        env::block_timestamp() / NANOS_PER_SEC
    }

    /// Current UTC day (days since Unix epoch).
    pub(crate) fn today(&self) -> u64 {
        self.now_sec() / SECONDS_PER_DAY
    }

    /// True if the caller is the configured agent.
    pub(crate) fn is_agent(&self) -> bool {
        env::predecessor_account_id() == self.agent
    }

    /// True if the caller is the administrator.
    pub(crate) fn is_admin(&self) -> bool {
        env::predecessor_account_id() == self.admin
    }

    /// Spendable internal funds under current policy:
    /// `ledger - total_reserved - reserve_floor - operations headroom`.
    ///
    /// Saturating subtraction is intentional: if the floor exceeds the
    /// ledger, available funds are simply zero and payments are refused.
    pub(crate) fn available_funds(&self) -> u128 {
        let unreserved = self.ledger_balance.saturating_sub(self.total_reserved);
        let above_floor = unreserved.saturating_sub(self.reserve_floor);
        above_floor.saturating_sub(OPERATIONS_HEADROOM_YOCTONEAR)
    }

    /// Rolls the daily window to today (if needed) and prunes stale
    /// day entries. Returns today's UTC day.
    ///
    /// This must be called before any daily-limit check or reservation.
    /// It is deliberately a `&mut self` operation and therefore never
    /// called from views.
    pub(crate) fn ensure_today(&mut self) -> u64 {
        let today = self.today();
        if today != self.current_day {
            self.current_day = today;
            self.trim_old_days(today);
        }
        today
    }

    /// Removes per-day accounting entries for days strictly older than
    /// `today`, but only for days whose reservations have all been
    /// finalized (`reserved == 0`). Days with outstanding reservations
    /// are kept until their reservations resolve, so cross-midnight
    /// finalization always finds its original day entry.
    ///
    /// Historical note: daily `spent` totals for past days are dropped
    /// when the entry is pruned. Per-payment records (with their
    /// accounting day) are retained in the bounded payment history.
    fn trim_old_days(&mut self, today: u64) {
        for _ in 0..MAX_DAY_TRIM_ITERATIONS {
            if self.oldest_day >= today {
                break;
            }
            let day = self.oldest_day;
            match self.daily.get(&day) {
                Some(entry) => {
                    if entry.reserved == 0 {
                        self.daily.remove(&day);
                        self.oldest_day = day.saturating_add(1);
                    } else {
                        // Outstanding reservation on this day: stop trimming.
                        // Later days are retried on the next roll.
                        break;
                    }
                }
                None => {
                    // No entry for this day: advance the marker.
                    self.oldest_day = day.saturating_add(1);
                }
            }
        }
    }

    /// Policy checks shared by both payment paths *before* any state
    /// mutation. Returns the UTC day the reservation must be booked to.
    ///
    /// Checks: per-payment maximum, daily limit (including existing
    /// reservations), available internal funds, and physical unlocked
    /// account balance (defense in depth).
    pub(crate) fn check_payment_policy(&mut self, amount: u128) -> u64 {
        require!(amount > 0, "E_ZERO_AMOUNT");
        require!(amount <= self.max_payment, "E_OVER_MAX_PAYMENT");

        let today = self.ensure_today();
        let entry = self.daily.get(&today).unwrap_or_default();
        let day_used = entry
            .spent
            .checked_add(entry.reserved)
            .unwrap_or_else(|| env::panic_str("E_DAY_OVERFLOW"));
        let with_amount = day_used
            .checked_add(amount)
            .unwrap_or_else(|| env::panic_str("E_DAY_OVERFLOW"));
        require!(with_amount <= self.daily_limit, "E_OVER_DAILY_LIMIT");

        let available = self.available_funds();
        require!(available >= amount, "E_INSUFFICIENT_FUNDS");

        // Defense in depth: the contract account must physically hold
        // enough *unlocked* balance to cover all in-flight transfers
        // plus the operations headroom. The internal ledger already
        // enforces this under normal operation (deposits are the only
        // source of ledger credit and there are no withdrawal paths),
        // but we check the chain-level truth as well and fail closed.
        let unlocked = env::account_balance().as_yoctonear()
            .saturating_sub(env::account_locked_balance().as_yoctonear());
        let reserved_after = self
            .total_reserved
            .checked_add(amount)
            .unwrap_or_else(|| env::panic_str("E_OVERFLOW"));
        require!(
            unlocked.saturating_sub(reserved_after) >= OPERATIONS_HEADROOM_YOCTONEAR,
            "E_PHYSICAL_FUNDS"
        );

        today
    }

    /// Reserves `amount` for a payment booked to `day`.
    /// Must only be called after [`Contract::check_payment_policy`] passed
    /// for the same amount, in the same transaction.
    pub(crate) fn reserve_funds(&mut self, amount: u128, day: u64) {
        let mut entry = self.daily.get(&day).unwrap_or_default();
        entry.reserved = entry
            .reserved
            .checked_add(amount)
            .unwrap_or_else(|| env::panic_str("E_OVERFLOW"));
        self.daily.insert(&day, &entry);
        self.total_reserved = self
            .total_reserved
            .checked_add(amount)
            .unwrap_or_else(|| env::panic_str("E_OVERFLOW"));
    }

    /// Applies the transfer outcome to the ledger and daily accounting.
    ///
    /// Uses the payment's *original* day, so a payment that starts before
    /// midnight and finalizes after midnight is still attributed to the
    /// original day.
    ///
    /// - Success: reserved[day] and total_reserved decrease, ledger
    ///   decreases, spent[day] increases.
    /// - Failure: reserved[day] and total_reserved decrease, ledger
    ///   unchanged.
    pub(crate) fn apply_transfer_outcome(&mut self, payment: &Payment, success: bool) {
        let day = payment.accounting_day;
        let amount = payment.amount;

        let mut entry = self
            .daily
            .get(&day)
            .unwrap_or_else(|| env::panic_str("E_DAY_ENTRY_MISSING"));

        // Fail closed on any accounting inconsistency: better to abort
        // than to corrupt the books.
        entry.reserved = entry
            .reserved
            .checked_sub(amount)
            .unwrap_or_else(|| env::panic_str("E_UNDERFLOW"));
        self.total_reserved = self
            .total_reserved
            .checked_sub(amount)
            .unwrap_or_else(|| env::panic_str("E_UNDERFLOW"));

        if success {
            entry.spent = entry
                .spent
                .checked_add(amount)
                .unwrap_or_else(|| env::panic_str("E_OVERFLOW"));
            self.ledger_balance = self
                .ledger_balance
                .checked_sub(amount)
                .unwrap_or_else(|| env::panic_str("E_UNDERFLOW"));
        }

        self.daily.insert(&day, &entry);
    }

    // ------------------------------------------------------------------
    // Public treasury methods
    // ------------------------------------------------------------------

    /// Fund the internal ledger. Payable; anyone may fund.
    ///
    /// Only the actually attached deposit is credited. An agent can never
    /// fabricate ledger credit.
    #[payable]
    pub fn fund(&mut self) {
        let deposit = env::attached_deposit().as_yoctonear();
        require!(deposit > 0, "E_ZERO_DEPOSIT");
        self.ledger_balance = self
            .ledger_balance
            .checked_add(deposit)
            .unwrap_or_else(|| env::panic_str("E_OVERFLOW"));
        crate::events::InvoiceGuardEvent::TreasuryFunded {
            amount: deposit.into(),
            ledger_balance: self.ledger_balance.into(),
        }
        .emit();
    }

    /// Convert a yoctoNEAR `u128` to `NearToken` for transfers.
    pub(crate) fn near_token(amount: u128) -> NearToken {
        NearToken::from_yoctonear(amount)
    }
}
