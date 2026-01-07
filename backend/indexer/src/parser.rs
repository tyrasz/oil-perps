use std::str::FromStr;

pub enum PerpsEvent {
    PositionOpened {
        owner: String,
        size: u64,
        side: bool,
        entry_price: u64,
    },
    PositionClosed {
        owner: String,
        pnl: i64,
        settlement: u64,
    },
    Liquidation {
        owner: String,
        size: u64,
        reward: u64,
    },
    FundingUpdated {
        rate: i64,
        long_oi: u64,
        short_oi: u64,
    },
}

pub fn parse_log(log: &str) -> Option<PerpsEvent> {
    // Parse Anchor program logs
    // Format: "Program log: Position opened: LONG 1000 @ 75000000 with 10x leverage"
    if log.contains("Position opened:") {
        return parse_position_opened(log);
    }

    if log.contains("Position closed:") {
        return parse_position_closed(log);
    }

    if log.contains("Position liquidated:") {
        return parse_liquidation(log);
    }

    if log.contains("Funding updated:") {
        return parse_funding_update(log);
    }

    None
}

fn parse_position_opened(log: &str) -> Option<PerpsEvent> {
    // Example: "Program log: Position opened: LONG 1000 @ 75000000 with 10x leverage"
    let parts: Vec<&str> = log.split_whitespace().collect();

    if parts.len() >= 8 {
        let side = parts.iter().position(|&p| p == "LONG" || p == "SHORT")?;
        let is_long = parts[side] == "LONG";

        let size_idx = side + 1;
        let size = parts.get(size_idx)?.parse::<u64>().ok()?;

        let price_idx = parts.iter().position(|&p| p == "@")? + 1;
        let price = parts.get(price_idx)?.parse::<u64>().ok()?;

        return Some(PerpsEvent::PositionOpened {
            owner: String::new(), // Would need to extract from transaction
            size,
            side: is_long,
            entry_price: price,
        });
    }

    None
}

fn parse_position_closed(log: &str) -> Option<PerpsEvent> {
    // Example: "Program log: Position closed: PnL=1000, Funding=-50, Fee=10, Settlement=10940"
    let pnl_start = log.find("PnL=")? + 4;
    let pnl_end = log[pnl_start..].find(',')?;
    let pnl = log[pnl_start..pnl_start + pnl_end].parse::<i64>().ok()?;

    let settlement_start = log.find("Settlement=")? + 11;
    let settlement = log[settlement_start..].trim().parse::<u64>().ok()?;

    Some(PerpsEvent::PositionClosed {
        owner: String::new(),
        pnl,
        settlement,
    })
}

fn parse_liquidation(log: &str) -> Option<PerpsEvent> {
    // Example: "Program log: Position liquidated: owner=xxx, size=1000, reward=50, insurance=100"
    let size_start = log.find("size=")? + 5;
    let size_end = log[size_start..].find(',')?;
    let size = log[size_start..size_start + size_end].parse::<u64>().ok()?;

    let reward_start = log.find("reward=")? + 7;
    let reward_end = log[reward_start..].find(',').unwrap_or(log[reward_start..].len());
    let reward = log[reward_start..reward_start + reward_end].parse::<u64>().ok()?;

    Some(PerpsEvent::Liquidation {
        owner: String::new(),
        size,
        reward,
    })
}

fn parse_funding_update(log: &str) -> Option<PerpsEvent> {
    // Example: "Program log: Funding updated: rate=100, long_oi=1000000, short_oi=800000, price=75000000"
    let rate_start = log.find("rate=")? + 5;
    let rate_end = log[rate_start..].find(',')?;
    let rate = log[rate_start..rate_start + rate_end].parse::<i64>().ok()?;

    let long_start = log.find("long_oi=")? + 8;
    let long_end = log[long_start..].find(',')?;
    let long_oi = log[long_start..long_start + long_end].parse::<u64>().ok()?;

    let short_start = log.find("short_oi=")? + 9;
    let short_end = log[short_start..].find(',').unwrap_or(log[short_start..].len());
    let short_oi = log[short_start..short_start + short_end].parse::<u64>().ok()?;

    Some(PerpsEvent::FundingUpdated {
        rate,
        long_oi,
        short_oi,
    })
}
