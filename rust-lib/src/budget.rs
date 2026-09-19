//! Aggregate call budgets. A per-call timeout bounds one round trip; a method making several is
//! bounded only by their SUM, which is what the view waits. A `Budget` is one allowance shared by
//! every outbound call of one entry point. Clock-free arithmetic, so plain cargo tests it.

use std::time::{Duration, Instant};

/// Reading a dependency's config or registry, or writing its defaults: local work.
pub const PROBE_BUDGET: Duration = Duration::from_millis(1500);
pub const INIT_BUDGET: Duration = Duration::from_secs(5);
/// One token_list or keystore read, or a relayed `list_assets`: local, but a catalogue is tens
/// of kilobytes of JSON.
pub const LOCAL_BUDGET: Duration = Duration::from_secs(3);

pub const STARTUP_BUDGET: Duration = Duration::from_secs(6);
pub const READ_BUDGET: Duration = Duration::from_secs(4);
/// `tokens` and `catalogue`: seeding eth_rpc and token_list on a fresh profile, then two local
/// reads.
pub const TOKENS_BUDGET: Duration = Duration::from_secs(8);
pub const ACCOUNTS_BUDGET: Duration = Duration::from_secs(6);
/// A verdict polled every five seconds must not outlast its own interval.
pub const VERDICT_BUDGET: Duration = Duration::from_secs(2);
pub const FEES_BUDGET: Duration = Duration::from_secs(5);
/// `balances`: the offered read, then evm_assets' own read, which it bounds at fourteen seconds.
pub const BALANCES_BUDGET: Duration = Duration::from_secs(17);
pub const ASSETS_BUDGET: Duration = Duration::from_secs(15);

/// One `build_swap`: uniswap_module gives its Multicall3 read fifteen seconds of its own.
pub const BUILD_BUDGET: Duration = Duration::from_secs(16);
/// One `prepare` or `send`: the sender's own cap, shrunk to what is left by `deadlineMs`.
pub const SENDER_BUDGET: Duration = Duration::from_secs(18);
/// A quote or a swap: the build, then the sender, as one allowance.
pub const QUOTE_BUDGET: Duration = Duration::from_secs(34);
pub const SWAP_BUDGET: Duration = Duration::from_secs(34);
/// One relayed `send_status`: the sender collects the signatures and broadcasts on this call.
pub const STATUS_BUDGET: Duration = Duration::from_secs(18);
/// The sender cancels locally, then tells the keystore within three seconds of its own.
pub const CANCEL_BUDGET: Duration = Duration::from_secs(5);
/// One relayed `history`: the sender's own receipt sweep is bounded at ten seconds.
pub const HISTORY_BUDGET: Duration = Duration::from_secs(12);

/// What the view's transport waits for one call, and for a quote or a swap.
pub const VIEW_CALL: Duration = Duration::from_secs(20);
pub const VIEW_SWAP_CALL: Duration = Duration::from_secs(35);

/// Below this a grant buys nothing, and the protocol refuses a sub-millisecond bound outright.
pub const MIN_SLICE: Duration = Duration::from_millis(50);
const CALLEE_MARGIN: Duration = Duration::from_millis(300);

/// The deadline to hand a callee this caller will wait `transport` for: shorter, so the
/// callee's own error sentence comes home rather than a bare transport timeout.
pub fn callee_deadline(transport: Duration) -> Option<i64> {
    transport
        .checked_sub(CALLEE_MARGIN)
        .filter(|d| *d >= MIN_SLICE)
        .map(|d| d.as_millis() as i64)
}

/// A shrinking allowance shared by every outbound call on one entry point.
pub struct Budget {
    started: Instant,
    total: Duration,
}

impl Budget {
    pub fn new(total: Duration) -> Self {
        Self { started: Instant::now(), total }
    }

    /// What the next call may spend, or `None` once too little is left to be worth a call.
    pub fn take(&self, per_call: Duration) -> Option<Duration> {
        slice(self.total, self.started.elapsed(), per_call)
    }
}

/// The whole policy, clock-free. A grant never exceeds what is left.
pub fn slice(total: Duration, elapsed: Duration, per_call: Duration) -> Option<Duration> {
    let grant = total.checked_sub(elapsed)?.min(per_call);
    (grant >= MIN_SLICE).then_some(grant)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_point_answers_inside_the_views_transport() {
        for b in [READ_BUDGET, TOKENS_BUDGET, ACCOUNTS_BUDGET, VERDICT_BUDGET, FEES_BUDGET,
                  BALANCES_BUDGET, STATUS_BUDGET, CANCEL_BUDGET, HISTORY_BUDGET] {
            assert!(b < VIEW_CALL, "{b:?}");
        }
        assert!(QUOTE_BUDGET < VIEW_SWAP_CALL && SWAP_BUDGET < VIEW_SWAP_CALL);
    }

    #[test]
    fn a_build_that_spends_its_whole_allowance_still_leaves_the_sender_time() {
        let left = QUOTE_BUDGET - BUILD_BUDGET;
        assert!(left >= SENDER_BUDGET, "a quote prices even after the slowest build");
        let left = SWAP_BUDGET - PROBE_BUDGET - BUILD_BUDGET;
        assert!(callee_deadline(slice(SWAP_BUDGET, SWAP_BUDGET - left, SENDER_BUDGET).unwrap()).unwrap() > 10_000);
    }

    #[test]
    fn a_grant_never_exceeds_what_is_left_and_a_sliver_is_nothing() {
        let total = Duration::from_secs(4);
        assert_eq!(slice(total, Duration::ZERO, PROBE_BUDGET), Some(PROBE_BUDGET));
        assert_eq!(slice(total, Duration::from_millis(3_500), PROBE_BUDGET), Some(Duration::from_millis(500)));
        assert_eq!(slice(total, Duration::from_millis(3_990), PROBE_BUDGET), None);
        assert_eq!(slice(total, Duration::from_secs(5), PROBE_BUDGET), None);
        assert_eq!(callee_deadline(Duration::from_millis(300)), None);
        assert_eq!(callee_deadline(Duration::from_secs(3)), Some(2_700));
    }
}
