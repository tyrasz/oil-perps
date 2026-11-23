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

/// How a position ended, with what was observed on chain when it did.
#[derive(Clone, Copy, Debug)]
pub struct Settlement {
    pub liquidated: bool,
    /// Tokens the vault paid out: the owner's payout, or the liquidator's reward
    pub paid: i128,
    /// Observed change of market.insurance_fund
    pub insurance_delta: i128,
    /// Collateral at settlement (add_margin included)
    pub collateral: u64,
    pub mark: u64,
    pub cumulative_funding: i64,
    /// Bad debt the fund could not absorb (model)
    pub uncovered: u64,
}

pub struct TrackedPosition {
    pub address: Pubkey,
    pub owner: usize,
    pub route: Route,
    pub pair: Option<usize>,
    pub opened_tick: u64,
    /// Last state read from chain
    pub state: Position,
    /// Where the vault became this position's counterparty: (price, cumulative
    /// funding). The entry for house positions; the partner's settlement mark
    /// for a venue leg whose partner has settled; None while paired.
    pub vault_basis: Option<(u64, i64)>,
    pub settled: Option<Settlement>,
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
        let vault_basis = (route == Route::House).then_some((state.entry_price, state.last_funding_payment));
        self.positions.push(TrackedPosition { address, owner, route, pair: None, opened_tick: tick, state, vault_basis, settled: None });
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

    pub fn partner(&self, idx: usize) -> Option<usize> {
        let p = &self.pairs[self.positions[idx].pair?];
        Some(if p.taker == idx { p.maker } else { p.taker })
    }

    /// Record a new state for `idx`; drops it from the open set once it is not open.
    pub fn update(&mut self, idx: usize, state: Position) {
        let open = state.status == PositionStatus::Open;
        self.positions[idx].state = state;
        if !open {
            self.open.retain(|&i| i != idx);
        }
    }

    /// `idx` settled: record how, and hand its partner (if still open) to the vault.
    pub fn settle(&mut self, idx: usize, s: Settlement) {
        self.positions[idx].settled = Some(s);
        if let Some(other) = self.partner(idx) {
            if self.positions[other].is_open() && self.positions[other].vault_basis.is_none() {
                self.positions[other].vault_basis = Some((s.mark, s.cumulative_funding));
            }
        }
    }

    pub fn open_of(&self, owner: usize) -> Vec<usize> {
        self.open.iter().copied().filter(|&i| self.positions[i].owner == owner).collect()
    }

    /// The vault's exposure as counterparty of unpaired open positions: (their
    /// net signed size, what the vault owes them at `mark` since it took them
    /// over, how many).
    pub fn vault_exposure(&self, mark: u64, cumulative_funding: i64) -> (i128, i128, usize) {
        let mut out = (0i128, 0i128, 0usize);
        for &idx in &self.open {
            let t = &self.positions[idx];
            let Some((basis, basis_funding)) = t.vault_basis else { continue };
            let p = &t.state;
            out.0 += t.signed_size();
            out.1 += crate::model::pnl(p.side, p.size, basis, mark) + crate::model::funding(p.side, p.size, basis_funding, cumulative_funding);
            out.2 += 1;
        }
        out
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
