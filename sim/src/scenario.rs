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
//!   and the admin settles it at the end. The LP pool's probes
//!   (adversary.rs) script their own state: a flash move inside a session, a
//!   self-crossed pair pushing the net to each cap, and a 30% gap at one
//!   daily reopen under a pair sized to leave bad debt pending.
//!
//! Every scenario has the public LP agents (agents.rs).

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

    /// Engine paths each scenario exists to reach. A run of at least two
    /// days that misses one fails invariant 16 (coverage); shorter runs only
    /// report it. Paths a seed can legitimately miss in two days are left out
    /// (documented where they would be).
    pub fn required_paths(self) -> &'static [&'static str] {
        match self {
            Scenario::Normal => &[
                "fill.mm_registry", "fill.orderbook", "open_position", "open_position.skew_impact", "close", "close.skew_impact",
                // "liquidate" is not required here: two days of normal trading
                // may have no liquidation (seed 1 has none); stress requires it
                "update_funding", "settle_position_pnl", "lp_deposit", "lp_request", "lp_withdraw", "lp_cancel",
                "reject.LpFlowWindowClosed", "probe.lp_withdraw_in_q", "probe.lp_withdraw_expired",
            ],
            Scenario::Stress => &[
                "close.deficit", "liquidate.deficit", "liquidate.reward_top_up", "bad_debt.lp_absorbed", "liquidate.stage1_partial",
                "match.split_by_cap", "reject.FillTooLarge", "order.fills>1",
            ],
            Scenario::Adversarial => &[
                "collusion.round", "wash.round", "reject.ApprovalSideNotAllowed", "reject.ApprovalExpired",
                "reject.ApprovalAllowanceExceeded", "reject.SelfTrade", "add_margin", "pause_market",
                "reject.MarketPaused", "settle_market", "close.at_settlement", "withdraw.with_open_positions",
                // The pool's probes (adversary.rs): each one's outcome checked (invariant 13)
                "probe.lp_params_w4d_q1d_rejected", "probe.lp_params_q23h_rejected", "probe.lp_params_boundary_accepted",
                "probe.adl_mode1_unauthorized", "probe.lp_deposit_funding_stale", "probe.funding_catchup_open", "probe.funding_catchup_close",
                "probe.lp_sandwich_donation", "probe.self_liquidation_loses", "probe.lp_sandwich_liquidation",
                "probe.selfcross_close_net_cap", "probe.house_open_net_cap", "probe.request_then_mode1_adl",
                "probe.winner_close_in_quiet", "probe.lp_deposit_in_q", "probe.adl_mode0_below_floor", "probe.opposite_winner_haircut",
                "probe.lp_withdraw_adl_required", "probe.self_adl_in_quiet", "probe.lp_withdraw_in_w", "probe.dust_pending_slice",
                "probe.selfcross_close_weekend_cap", "probe.house_open_weekend_cap",
                "probe.settled_winner_close_quiet", "probe.settled_owner_close_quiet", "probe.lp_sandwich_sweep",
                "probe.settled_lp_withdraw_quiet",
                "bad_debt.pending", "adl.mode0", "donate_to_pool",
            ],
        }
    }
}
