//! Compile-time constants, limits, and storage key allocation.
//!
//! Every dynamic collection uses a unique storage prefix via [`StorageKey`]
//! so that collections can never stomp on each other's keys.

/// Protocol/domain identifier embedded in every signed invoice.
/// Part of the signed payload (domain separation).
pub const PROTOCOL_ID: &str = "invoiceguard";

/// Protocol version embedded in every signed invoice.
/// Bump only when the signed payload format changes; old signatures
/// will then be rejected (which is the point).
pub const PROTOCOL_VERSION: u32 = 1;

/// Maximum number of registered vendors (bounded storage).
pub const MAX_VENDORS: u64 = 64;

/// Maximum number of simultaneously pending manual payment requests.
pub const MAX_PENDING_REQUESTS: u64 = 32;

/// Maximum memo length in bytes for manual payment requests.
pub const MAX_MEMO_LEN: usize = 256;

/// Maximum retained payment records (bounded history window).
/// Terminal payments older than this window are deleted;
/// replay protection (vendor nonces) is NOT affected by this trimming.
pub const MAX_RETAINED_PAYMENTS: u64 = 100;

/// Default validity window of a manual payment request before it
/// can no longer be approved. 24 hours, in seconds (UTC).
pub const DEFAULT_REQUEST_TTL_SEC: u64 = 24 * 60 * 60;

/// Upper bound for the admin-configurable request TTL.
pub const MAX_REQUEST_TTL_SEC: u64 = 30 * 24 * 60 * 60;

/// Maximum accepted length of the network identifier at init.
pub const MAX_NETWORK_ID_LEN: usize = 32;

/// Operations headroom (yoctoNEAR) always kept between the contract's
/// spendable internal budget and what may be promised to vendors.
///
/// `available = ledger - reserved - reserve_floor - OPERATIONS_HEADROOM`.
///
/// SIMPLIFICATION, NOT A GUARANTEE. This is a fixed compile-time constant:
/// it is not derived from `env::storage_usage()`, not adjusted for actual
/// gas prices, and not proven sufficient for every load. It is an
/// engineering estimate for this Testnet prototype, based on the bounded
/// collections (at most `MAX_VENDORS` vendors, `MAX_PENDING_REQUESTS`
/// pending requests and `MAX_RETAINED_PAYMENTS` retained payments).
/// NEAR storage staking costs 10^19 yoctoNEAR per byte, so 1 NEAR covers
/// roughly 100 KB of state; the normal retained state is well below that.
///
/// The bound is NOT unconditional: `trim_old_days` stops at the first day
/// with an outstanding reservation, and `trim_payment_history` stops at the
/// first non-terminal payment. A payment stuck in `PROCESSING` (e.g. the
/// fail-closed callback path) therefore lets daily entries accumulate and
/// can, over a long enough period, erode this buffer. A production design
/// would compute the reserve from `env::storage_usage()` and would provide
/// a way to resolve stuck reservations.
pub const OPERATIONS_HEADROOM_YOCTONEAR: u128 = 10_u128.pow(24); // 1 NEAR

/// Gas attached to the private transfer-finalization callback.
/// The callback performs a few map reads/writes and emits an event;
/// 30 Tgas is a generous bound.
pub const CALLBACK_GAS_TERAGAS: u64 = 30;

/// Upper bound for reading a promise result in the callback.
/// `Promise::transfer` returns no data, so successful results are empty;
/// anything longer than this is treated as an unexpected protocol
/// condition and fails closed.
pub const MAX_PROMISE_RESULT_LEN: usize = 16;

/// Maximum number of stale UTC-day accounting entries removed per
/// treasury operation (defensive bound on the trim loop).
pub const MAX_DAY_TRIM_ITERATIONS: u32 = 512;

/// Maximum number of payment records returned by a single history query.
pub const MAX_QUERY_LIMIT: u64 = 100;

/// Seconds per UTC day (days are counted since the Unix epoch, UTC).
pub const SECONDS_PER_DAY: u64 = 86_400;

/// Nanoseconds per second. `env::block_timestamp()` is nanoseconds;
/// all externally documented timestamps are Unix seconds (UTC).
pub const NANOS_PER_SEC: u64 = 1_000_000_000;
