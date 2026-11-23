//! Which error, if any, each instruction the simulation sends must fail with,
//! predicted from the model and the spec's check order (docs/venue-settlement.md
//! 1.5, 5.3, 6.2 and the handlers' documented gates). Every transaction carries
//! its prediction: a failure with any other error is unexpected (invariant 6),
//! and a predicted rejection that lands is a violation (invariant 10).

use crate::model;
use perps_core::instructions::VenueApprovalParams;
use perps_core::state::{Market, Position, Side, Venue, VenueApproval};

/// `(program, error)` as the chain decoder names a failure.
pub type Rej = (&'static str, &'static str);

pub const fn core(code: &'static str) -> Rej {
    ("perps_core", code)
}

/// Read-price staleness limit of open, close, liquidate and funding (price.rs).
pub const MAX_PRICE_AGE_SECS: u64 = 60;
/// The confidence limit when the market sets none.
pub const DEFAULT_MAX_CONFIDENCE_BPS: u32 = 100;

/// The oracle as last published.
#[derive(Clone, Copy, Debug)]
pub struct OracleView {
    pub price: u64,
    pub conf: u64,
    pub published_at: i64,
}

pub fn oracle(m: &Market, o: &OracleView, now: i64, max_age: u64) -> Option<Rej> {
    if (o.published_at - now).unsigned_abs() > max_age {
        return Some(core("StaleOraclePrice"));
    }
    let limit = if m.max_confidence_bps == 0 { DEFAULT_MAX_CONFIDENCE_BPS } else { m.max_confidence_bps };
    if o.conf as u128 * 10_000 > o.price as u128 * limit as u128 {
        return Some(core("OracleConfidenceTooWide"));
    }
    if o.price < m.min_oracle_price {
        return Some(core("OraclePriceBelowFloor"));
    }
    None
}

fn settled(m: &Market) -> bool {
    m.settlement_price > 0
}

pub fn open_position(m: &Market, o: &OracleView, now: i64, free: u64, side: Side, size: u64, leverage: u32) -> Option<Rej> {
    if m.is_paused {
        return Some(core("MarketPaused"));
    }
    if leverage == 0 || leverage > m.max_leverage {
        return Some(core("ExcessiveLeverage"));
    }
    if settled(m) {
        return Some(core("MarketSettled"));
    }
    if !model::in_session(m, now) {
        return Some(core("MarketClosed"));
    }
    if let Some(r) = oracle(m, o, now, MAX_PRICE_AGE_SECS) {
        return Some(r);
    }
    let Some(required) = model::open_margin(m, size, o.price, leverage, now) else { return Some(core("ExcessiveLeverage")) };
    if free < required {
        return Some(core("InsufficientCollateral"));
    }
    let oi = match side {
        Side::Long => m.long_open_interest,
        Side::Short => m.short_open_interest,
    };
    if oi.checked_add(size).map_or(true, |x| x > m.max_open_interest) {
        return Some(core("OpenInterestCapExceeded"));
    }
    None
}

/// close_position of an open position: a settled market closes at its
/// settlement price at any time; otherwise session, then oracle.
pub fn close_position(m: &Market, o: &OracleView, now: i64) -> Option<Rej> {
    if settled(m) {
        return None;
    }
    if !model::in_session(m, now) {
        return Some(core("MarketClosed"));
    }
    oracle(m, o, now, MAX_PRICE_AGE_SECS)
}

/// The price close_position settles at.
pub fn close_mark(m: &Market, o: &OracleView) -> u64 {
    if settled(m) {
        m.settlement_price
    } else {
        o.price
    }
}

pub fn liquidate(m: &Market, o: &OracleView, now: i64, p: &Position) -> Option<Rej> {
    if settled(m) {
        return Some(core("MarketSettled"));
    }
    if !model::in_session(m, now) {
        return Some(core("MarketClosed"));
    }
    if let Some(r) = oracle(m, o, now, MAX_PRICE_AGE_SECS) {
        return Some(r);
    }
    if !model::liquidatable(m, p, o.price, now) {
        return Some(core("NotLiquidatable"));
    }
    None
}

pub fn update_funding(m: &Market, o: &OracleView, now: i64) -> Option<Rej> {
    if now - m.last_funding_time < m.funding_interval {
        return Some(core("InvalidMarketConfig"));
    }
    if settled(m) {
        return Some(core("MarketSettled"));
    }
    if !model::in_session(m, now) {
        return Some(core("MarketClosed"));
    }
    oracle(m, o, now, MAX_PRICE_AGE_SECS)
}

/// withdraw_collateral of `amount` from `free`, paid from a vault holding `vault`.
pub fn withdraw(free: u64, amount: u64, vault: u64) -> Option<Rej> {
    if free < amount {
        return Some(core("InsufficientCollateral"));
    }
    if vault < amount {
        // The vault is short: the realised form of the solvency finding
        return Some(("spl_token", "Custom(1)"));
    }
    None
}

pub fn add_margin(free: u64, amount: u64) -> Option<Rej> {
    if free < amount {
        return Some(core("InsufficientCollateral"));
    }
    None
}

/// An approval's bounds, as written by approve or update, or as stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terms {
    pub allowance: u64,
    pub max_leverage: u32,
    pub max_buy_price: u64,
    pub min_sell_price: u64,
    pub max_price_deviation_bps: u16,
    pub expires_at: i64,
    pub allowed_sides: u8,
}

impl From<&VenueApprovalParams> for Terms {
    fn from(p: &VenueApprovalParams) -> Terms {
        Terms {
            allowance: p.allowance, max_leverage: p.max_leverage, max_buy_price: p.max_buy_price, min_sell_price: p.min_sell_price,
            max_price_deviation_bps: p.max_price_deviation_bps, expires_at: p.expires_at, allowed_sides: p.allowed_sides,
        }
    }
}

impl From<&VenueApproval> for Terms {
    fn from(a: &VenueApproval) -> Terms {
        Terms {
            allowance: a.allowance, max_leverage: a.max_leverage, max_buy_price: a.max_buy_price, min_sell_price: a.min_sell_price,
            max_price_deviation_bps: a.max_price_deviation_bps, expires_at: a.expires_at, allowed_sides: a.allowed_sides,
        }
    }
}

/// approve_venue / update_venue_approval parameter validation.
pub fn approval_params(m: &Market, t: &Terms, now: i64) -> Option<Rej> {
    let ok = t.max_leverage > 0
        && t.max_leverage <= m.max_leverage
        && t.max_price_deviation_bps <= 10_000
        && t.expires_at > now
        && t.expires_at <= now + 90 * 86_400
        && t.allowed_sides != 0
        && t.allowed_sides & !3 == 0;
    if ok {
        None
    } else {
        Some(core("InvalidApprovalParams"))
    }
}

/// One party of a venue fill.
#[derive(Clone, Copy)]
pub struct Leg {
    pub side: Side,
    pub leverage: u32,
    pub free: u64,
    /// The approval as it will stand when the fill runs (None: no account)
    pub approval: Option<Terms>,
}

/// The margin each leg posts, when the fill lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FillMargins {
    pub taker: u64,
    pub maker: u64,
}

/// The side bits of `allowed_sides`.
pub fn side_bit(side: Side) -> u8 {
    match side {
        Side::Long => 1,
        Side::Short => 2,
    }
}

/// take_margin for one leg (venue_fill runs it for the taker, then the maker).
pub fn leg(m: &Market, l: &Leg, size: u64, price: u64, oracle: u64, now: i64) -> Result<u64, Rej> {
    let a = l.approval.expect("checked by the caller");
    if l.leverage == 0 || l.leverage > m.max_leverage {
        return Err(core("ExcessiveLeverage"));
    }
    if l.leverage > a.max_leverage {
        return Err(core("ApprovalLeverageExceeded"));
    }
    if now > a.expires_at {
        return Err(core("ApprovalExpired"));
    }
    if a.allowed_sides & side_bit(l.side) == 0 {
        return Err(core("ApprovalSideNotAllowed"));
    }
    let within_limit = match l.side {
        Side::Long => a.max_buy_price == 0 || price <= a.max_buy_price,
        Side::Short => a.min_sell_price == 0 || price >= a.min_sell_price,
    };
    if !within_limit {
        return Err(core("ApprovalLimitPrice"));
    }
    if !model::within_adverse(l.side, price, oracle, a.max_price_deviation_bps) {
        return Err(core("ApprovalPriceDeviation"));
    }
    let c = model::venue_margin(m, l.side, size, price, oracle, l.leverage, now).ok_or(core("ExcessiveLeverage"))?;
    if l.free < c {
        return Err(core("InsufficientCollateral"));
    }
    if a.allowance < c {
        return Err(core("ApprovalAllowanceExceeded"));
    }
    Ok(c)
}

/// venue_fill, reached by CPI from a venue whose own checks passed. Account
/// constraints first (in field order), then the handler.
#[allow(clippy::too_many_arguments)]
pub fn venue_fill(
    m: &Market, fills_paused: bool, venue: &Venue, o: &OracleView, now: i64, signers: u8, self_trade: bool,
    size: u64, price: u64, taker: &Leg, maker: &Leg,
) -> Result<FillMargins, Rej> {
    if fills_paused {
        return Err(core("VenueFillsPaused"));
    }
    if m.is_paused {
        return Err(core("MarketPaused"));
    }
    if !venue.enabled {
        return Err(core("VenueDisabled"));
    }
    if taker.approval.is_none() {
        return Err(core("AccountNotInitialized"));
    }
    if self_trade {
        return Err(core("SelfTrade"));
    }
    if maker.approval.is_none() {
        return Err(core("AccountNotInitialized"));
    }
    if settled(m) {
        return Err(core("MarketSettled"));
    }
    if !model::in_session(m, now) {
        return Err(core("MarketClosed"));
    }
    if size == 0 || price == 0 {
        return Err(core("InvalidFillParams"));
    }
    if signers < venue.min_signers {
        return Err(core("MissingSigner"));
    }
    if let Some(r) = oracle(m, o, now, venue.max_oracle_age_secs as u64) {
        return Err(r);
    }
    if !model::within_band(price, o.price, venue.max_price_deviation_bps) {
        return Err(core("FillPriceOutOfBand"));
    }
    let n = model::notional(size, price);
    if n == 0 {
        return Err(core("InvalidFillParams"));
    }
    if n > venue.max_fill_notional as i128 {
        return Err(core("FillTooLarge"));
    }
    let cap = |oi: u64| oi.checked_add(size).map_or(false, |x| x <= m.max_open_interest);
    if !(cap(m.long_open_interest) && cap(m.short_open_interest)) {
        return Err(core("OpenInterestCapExceeded"));
    }
    let t = leg(m, taker, size, price, o.price, now)?;
    let k = leg(m, maker, size, price, o.price, now)?;
    Ok(FillMargins { taker: t, maker: k })
}

/// mm-registry fill_quote's own checks (spec 5.3), before its CPI. Returns
/// the fill size and price.
pub fn fill_quote(q: &mm_registry::TwoSidedQuote, now: i64, taker_is_mm: bool, size: u64, is_buy: bool, limit: u64) -> Result<(u64, u64), Rej> {
    let mm = |c| ("mm_registry", c);
    if !q.is_active {
        return Err(mm("QuoteNotActive"));
    }
    if q.expires_at > 0 && now > q.expires_at {
        return Err(mm("QuoteExpired"));
    }
    if !(q.leverage > 0 && q.expires_at > 0 && q.expires_at <= now + 86_400) {
        return Err(mm("QuoteNotFillable"));
    }
    if taker_is_mm {
        return Err(mm("SelfTrade"));
    }
    if limit == 0 {
        return Err(mm("InvalidPrice"));
    }
    let (price, remaining) = if is_buy { (q.ask_price, q.ask_remaining) } else { (q.bid_price, q.bid_remaining) };
    if remaining == 0 {
        return Err(mm("FillSizeExceedsRemaining"));
    }
    if !(if is_buy { price <= limit } else { price >= limit }) {
        return Err(mm("PriceWorseThanLimit"));
    }
    // allow_partial is always set
    let fill = size.min(remaining);
    if !(fill > 0 && (fill >= q.min_fill_size || fill == remaining)) {
        return Err(mm("FillSizeTooSmall"));
    }
    Ok((fill, price))
}

/// orderbook match_orders' own checks (spec 6.2, A5), before its CPI. Returns
/// the fill size.
pub fn match_orders(bid: &orderbook::Order, ask: &orderbook::Order, now: i64, max_fill_size: u64) -> Result<u64, Rej> {
    use orderbook::{OrderStatus, OrderType};
    let ob = |c| ("orderbook", c);
    for o in [bid, ask] {
        if !(o.status == OrderStatus::Open || o.status == OrderStatus::PartiallyFilled) {
            return Err(ob("InvalidOrder"));
        }
        if o.order_type != OrderType::Limit {
            return Err(ob("UnsupportedOrderType"));
        }
        if o.leverage == 0 {
            return Err(ob("LegacyOrder"));
        }
        if now > o.expires_at {
            return Err(ob("OrderExpired"));
        }
        if o.fills >= o.max_fills {
            return Err(ob("RentDepositExhausted"));
        }
    }
    if bid.owner == ask.owner {
        return Err(ob("SelfTrade"));
    }
    if bid.price < ask.price {
        return Err(ob("PriceMismatch"));
    }
    let (rb, ra) = (bid.size - bid.filled_size, ask.size - ask.filled_size);
    let crossing = rb.min(ra);
    let fill = if max_fill_size > 0 { crossing.min(max_fill_size) } else { crossing };
    if !((fill >= bid.min_fill_size || fill == rb) && (fill >= ask.min_fill_size || fill == ra)) {
        return Err(ob("FillTooSmall"));
    }
    Ok(fill)
}

/// Errors each instruction label may legitimately be predicted to fail with.
/// A prediction outside its label's set is a simulation bug and is reported
/// as unexpected; MarketClosed is only in the keeper's sets, for its passes at
/// session edges (agents act only in session).
pub fn allowed_for(label: &str) -> &'static [&'static str] {
    const ORACLE: [&str; 3] = ["StaleOraclePrice", "OracleConfidenceTooWide", "OraclePriceBelowFloor"];
    match label {
        "open_position" | "open_position_probe" => &[
            "MarketPaused", "ExcessiveLeverage", "MarketSettled", "StaleOraclePrice", "OracleConfidenceTooWide", "OraclePriceBelowFloor",
        ],
        "close_position" => &ORACLE,
        "liquidate" | "liquidate_probe" => &[
            "MarketSettled", "MarketClosed", "NotLiquidatable", "StaleOraclePrice", "OracleConfidenceTooWide", "OraclePriceBelowFloor",
        ],
        "update_funding" => &["MarketSettled", "MarketClosed", "StaleOraclePrice", "OracleConfidenceTooWide", "OraclePriceBelowFloor"],
        // The vault running dry is the realised solvency finding
        "withdraw" => &["Custom(1)"],
        "withdraw_probe" | "add_margin_probe" => &["InsufficientCollateral"],
        // A taker checks its own leg; it does not see the maker's approval or collateral
        "fill_quote" => &[
            "StaleOraclePrice", "OracleConfidenceTooWide", "OraclePriceBelowFloor",
            "ApprovalPriceDeviation", "ApprovalAllowanceExceeded", "InsufficientCollateral",
        ],
        "fill_quote_probe" => &[
            "ExcessiveLeverage", "FillTooLarge", "ApprovalSideNotAllowed", "ApprovalExpired", "ApprovalLeverageExceeded",
            "ApprovalLimitPrice", "ApprovalAllowanceExceeded", "ApprovalPriceDeviation", "AccountNotInitialized",
            "MarketPaused", "SelfTrade", "QuoteExpired", "PriceWorseThanLimit", "InsufficientCollateral",
            "StaleOraclePrice", "OracleConfidenceTooWide",
        ],
        // The keeper sends a predicted-failing cross once, as its simulate step
        "match_orders" => &[
            "StaleOraclePrice", "OracleConfidenceTooWide", "OraclePriceBelowFloor",
            "ApprovalAllowanceExceeded", "InsufficientCollateral", "ApprovalPriceDeviation",
        ],
        "match_orders_probe" => &["FillTooLarge", "SelfTrade", "MarketPaused", "StaleOraclePrice", "OracleConfidenceTooWide"],
        _ => &[],
    }
}
