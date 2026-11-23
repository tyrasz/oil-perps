//! What the simulation knows about on-chain objects it created. BanksClient
//! cannot enumerate program accounts, so every position, order, quote and
//! approval is recorded when the transaction that creates it lands, in
//! creation order (iteration never depends on key values, so a seed replays
//! exactly).

use crate::support::Pubkey;
use perps_core::state::{Position, PositionStatus, Side};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Route {
    /// open_position at the oracle, against the vault
    House,
    MmRegistry,
    Orderbook,
}

impl Route {
    pub fn name(self) -> &'static str {
        match self {
            Route::House => "house",
            Route::MmRegistry => "mm_registry",
            Route::Orderbook => "orderbook",
        }
    }
}

/// What a position's settlements realized, summed over every piece (a close,
/// a liquidation or its stage-1 slices, ADL slices, settle_position_pnl).
/// Per piece, and so in total (invariant 4):
/// `paid + fees − top_up == collateral + realized − haircut + deficit`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Settlement {
    pub liquidated: bool,
    /// Paid out of the position: to the owner (tokens or free collateral) and to liquidators
    pub paid: i128,
    /// Close fees and penalties kept (insurance, treasury, cash)
    pub fees: i128,
    /// Stage-2 liquidator reward the insurance fund paid
    pub top_up: i128,
    /// Collateral released (add_margin included)
    pub collateral: i128,
    /// pnl + funding realized, before haircuts
    pub realized: i128,
    /// Solvency, bad-debt and cash haircuts on profits
    pub haircut: i128,
    /// Negative equity booked through the waterfall
    pub deficit: i128,
    /// Insurance's own draw (deficits it paid, reward top-ups)
    pub insurance_draw: i128,
    /// Last piece: the price it settled at (exec), the oracle, cumulative funding
    pub mark: u64,
    pub oracle: u64,
    pub cumulative_funding: i64,
    pub pieces: u32,
    /// Fully settled (no size left)
    pub done: bool,
}

impl Settlement {
    pub fn balanced(&self) -> bool {
        self.paid + self.fees - self.top_up == self.collateral + self.realized - self.haircut + self.deficit
    }
}

pub struct TrackedPosition {
    pub address: Pubkey,
    pub owner: usize,
    pub route: Route,
    pub pair: Option<usize>,
    pub opened_tick: u64,
    /// Last state read from chain
    pub state: Position,
    pub settled: Settlement,
}

impl TrackedPosition {
    pub fn is_open(&self) -> bool {
        self.state.status == PositionStatus::Open
    }

    pub fn side(&self) -> Side {
        self.state.side
    }

    pub fn signed_size(&self) -> i128 {
        crate::model::sign(self.state.side) * self.state.size as i128
    }
}

/// The two legs of one venue fill.
pub struct Pair {
    pub taker: usize,
    pub maker: usize,
    /// Opened by the colluding pair (invariant 12)
    pub collusion: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OrderEnd {
    Filled,
    /// fills reached max_fills; any remainder was cancelled
    MaxFills,
    Cancelled,
    Expired,
}

pub struct TrackedOrder {
    pub address: Pubkey,
    pub owner: usize,
    pub price: u64,
    pub placed_tick: u64,
    pub expires_at: i64,
    /// Still an account on chain
    pub live: bool,
    pub fills: u32,
    /// Fills smaller than the cross because of the per-fill cap
    pub capped_fills: u32,
    pub end: Option<OrderEnd>,
}

/// A VenueApproval and exactly what it must hold: the owner's last grant,
/// minus the margin every fill since committed (invariant 5).
pub struct TrackedApproval {
    pub address: Pubkey,
    pub cap: u64,
    pub exists: bool,
}

#[derive(Default)]
pub struct Ledger {
    pub positions: Vec<TrackedPosition>,
    pub by_address: HashMap<Pubkey, usize>,
    /// Indices of positions believed open, in creation order
    pub open: Vec<usize>,
    pub pairs: Vec<Pair>,
    pub orders: Vec<TrackedOrder>,
    pub approvals: Vec<TrackedApproval>,
    pub approval_index: HashMap<Pubkey, usize>,
    /// Quote address -> fills, in creation order
    pub quote_fills: Vec<(Pubkey, u32)>,
}

impl Ledger {
    pub fn add_position(&mut self, address: Pubkey, owner: usize, route: Route, tick: u64, state: Position) -> usize {
        let idx = self.positions.len();
        self.positions.push(TrackedPosition { address, owner, route, pair: None, opened_tick: tick, state, settled: Settlement::default() });
        self.by_address.insert(address, idx);
        self.open.push(idx);
        idx
    }

    pub fn add_pair(&mut self, taker: usize, maker: usize, collusion: bool) -> usize {
        let pair = self.pairs.len();
        self.pairs.push(Pair { taker, maker, collusion });
        self.positions[taker].pair = Some(pair);
        self.positions[maker].pair = Some(pair);
        pair
    }

    /// Record a new state for `idx`; drops it from the open set once it is not open.
    pub fn update(&mut self, idx: usize, state: Position) {
        let open = state.status == PositionStatus::Open;
        self.positions[idx].state = state;
        if !open {
            self.open.retain(|&i| i != idx);
        }
    }

    /// One settlement piece of `idx` landed.
    pub fn settle(&mut self, idx: usize, piece: Settlement) {
        let s = &mut self.positions[idx].settled;
        s.liquidated |= piece.liquidated;
        s.paid += piece.paid;
        s.fees += piece.fees;
        s.top_up += piece.top_up;
        s.collateral += piece.collateral;
        s.realized += piece.realized;
        s.haircut += piece.haircut;
        s.deficit += piece.deficit;
        s.insurance_draw += piece.insurance_draw;
        s.mark = piece.mark;
        s.oracle = piece.oracle;
        s.cumulative_funding = piece.cumulative_funding;
        s.pieces += 1;
        s.done = piece.done;
    }

    pub fn open_of(&self, owner: usize) -> Vec<usize> {
        self.open.iter().copied().filter(|&i| self.positions[i].owner == owner).collect()
    }

    /// The owner granted `cap` (approve or update), or the account now holds it.
    pub fn track_approval(&mut self, address: Pubkey, cap: u64) {
        match self.approval_index.get(&address) {
            Some(&i) => {
                self.approvals[i].cap = cap;
                self.approvals[i].exists = true;
            }
            None => {
                self.approval_index.insert(address, self.approvals.len());
                self.approvals.push(TrackedApproval { address, cap, exists: true });
            }
        }
    }

    pub fn approval_cap(&self, address: &Pubkey) -> Option<u64> {
        self.approval_index.get(address).map(|&i| &self.approvals[i]).filter(|a| a.exists).map(|a| a.cap)
    }

    pub fn revoke_approval(&mut self, address: &Pubkey) {
        if let Some(&i) = self.approval_index.get(address) {
            self.approvals[i].exists = false;
        }
    }

    pub fn record_quote_fill(&mut self, quote: Pubkey) {
        match self.quote_fills.iter_mut().find(|(q, _)| *q == quote) {
            Some(e) => e.1 += 1,
            None => self.quote_fills.push((quote, 1)),
        }
    }

    pub fn record_quote(&mut self, quote: Pubkey) {
        if !self.quote_fills.iter().any(|(q, _)| *q == quote) {
            self.quote_fills.push((quote, 0));
        }
    }
}
