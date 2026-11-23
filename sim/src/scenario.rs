//! Scenarios (--scenario): what the market does and who trades in it.
//!
//! - normal: the default roster (2 MMs, momentum, mean-reversion, degen, limit
//!   and house traders) with 1-6% weekend gaps.
//! - stress: 10-20% gaps at every reopen (daily breaks and the weekend), 10x
//!   holders carried over the daily break, a whale taker and a deep MM whose
//!   clips exceed the $50k per-fill cap, and large limit orders the keeper must
//!   split. Bad debt (deficits, insurance top-ups, uncovered debt) happens here.
//! - adversarial: the normal roster plus a colluding pair crossing at the band
//!   edge at the maximum leverage and wash trading, and a prober that sends
//!   requests the engine must reject (excluded side, expired, exhausted and
//!   revoked approvals, self-trades, a paused market), add_margin and
//!   withdrawals with positions open; the guardian pauses the market mid-run
//!   and the admin settles it at the end.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    Normal,
    Stress,
    Adversarial,
}

impl Scenario {
    pub fn parse(s: &str) -> Option<Scenario> {
        match s {
            "normal" => Some(Scenario::Normal),
            "stress" => Some(Scenario::Stress),
            "adversarial" => Some(Scenario::Adversarial),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Scenario::Normal => "normal",
            Scenario::Stress => "stress",
            Scenario::Adversarial => "adversarial",
        }
    }

    /// Reopen gaps: (weekend range, daily-break range), as fractions; a daily
    /// range of None means a small normal move (0.25% sd).
    pub fn gaps(self) -> ((f64, f64), Option<(f64, f64)>) {
        match self {
            Scenario::Stress => ((0.10, 0.20), Some((0.10, 0.20))),
            _ => ((0.01, 0.06), None),
        }
    }

    /// Engine paths each scenario exists to reach; the report lists any that a
    /// run missed (with too few days or an unlucky seed some cannot happen).
    pub fn required_paths(self) -> &'static [&'static str] {
        match self {
            Scenario::Normal => &["fill.mm_registry", "fill.orderbook", "open_position", "close", "update_funding", "liquidate"],
            Scenario::Stress => &[
                "close.deficit", "liquidate.deficit", "liquidate.reward_top_up", "bad_debt.uncovered",
                "match.split_by_cap", "reject.FillTooLarge", "order.fills>1",
            ],
            Scenario::Adversarial => &[
                "collusion.round", "wash.round", "reject.ApprovalSideNotAllowed", "reject.ApprovalExpired",
                "reject.ApprovalAllowanceExceeded", "reject.SelfTrade", "add_margin", "pause_market",
                "reject.MarketPaused", "settle_market", "close.at_settlement", "withdraw.with_open_positions",
            ],
        }
    }
}
