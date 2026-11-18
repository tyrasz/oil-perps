// Pyth price conversion and validation, free of Anchor and program types so
// the keeper (backend/keeper) and prop-amm compile this same file via
// `#[path]` / `include!`. Keep it dependency-free.

/// Number of decimals used for all prices inside the program.
pub const PRICE_DECIMALS: i32 = 6;

/// Convert a Pyth price (`price * 10^expo`) into the program's 6-decimal
/// fixed-point format, i.e. `price * 10^(6 + expo)`.
///
/// Returns `None` if the price is not positive, the result overflows `u64`,
/// or the result rounds down to zero (an unusable price).
pub fn normalize_pyth_price(price: i64, expo: i32) -> Option<u64> {
    if price <= 0 {
        return None;
    }
    let price = price as u128;
    let shift = PRICE_DECIMALS.checked_add(expo)?;

    let normalized = if shift >= 0 {
        price.checked_mul(10u128.checked_pow(shift as u32)?)?
    } else {
        // 10^39 overflows u128; any shift that large rounds the price to zero.
        let divisor = 10u128.checked_pow(shift.unsigned_abs()).unwrap_or(u128::MAX);
        price / divisor
    };

    if normalized == 0 {
        return None;
    }
    u64::try_from(normalized).ok()
}

/// Oracle prices older than this are rejected.
pub const MAX_PRICE_AGE_SECS: u64 = 60;
/// Confidence limit used when a market has none configured: 1% of the price.
pub const DEFAULT_MAX_CONFIDENCE_BPS: u32 = 100;

#[derive(Debug, PartialEq, Eq)]
pub enum PriceRejection {
    /// Zero, negative, or not representable in 6 decimals.
    Invalid,
    /// Pyth's confidence interval is too wide relative to the price.
    ConfidenceTooWide,
    /// Below the market's configured minimum price.
    BelowFloor,
}

/// Check a Pyth price and convert it to 6 decimals.
///
/// `conf` shares the price's exponent, so the confidence ratio is `conf / price`.
/// `max_confidence_bps == 0` uses `DEFAULT_MAX_CONFIDENCE_BPS`;
/// `min_price == 0` disables the floor.
pub fn validate_price(
    price: i64,
    conf: u64,
    expo: i32,
    max_confidence_bps: u32,
    min_price: u64,
) -> core::result::Result<u64, PriceRejection> {
    let normalized = normalize_pyth_price(price, expo).ok_or(PriceRejection::Invalid)?;

    let limit = if max_confidence_bps == 0 { DEFAULT_MAX_CONFIDENCE_BPS } else { max_confidence_bps };
    if (conf as u128) * 10_000 > (price as u128) * limit as u128 {
        return Err(PriceRejection::ConfidenceTooWide);
    }
    if normalized < min_price {
        return Err(PriceRejection::BelowFloor);
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_tight_confidence() {
        // $75.00 +/- $0.05 (0.07%)
        assert_eq!(validate_price(7_500_000_000, 5_000_000, -8, 0, 0), Ok(75_000_000));
    }

    #[test]
    fn rejects_wide_confidence_with_default_limit() {
        // $75.00 +/- $0.75 is exactly 1%: allowed; one tick more is not
        assert_eq!(validate_price(7_500_000_000, 75_000_000, -8, 0, 0), Ok(75_000_000));
        assert_eq!(
            validate_price(7_500_000_000, 75_000_001, -8, 0, 0),
            Err(PriceRejection::ConfidenceTooWide)
        );
    }

    #[test]
    fn uses_configured_confidence_limit() {
        // 2% band allowed under a 3% limit, rejected under 1%
        assert!(validate_price(7_500_000_000, 150_000_000, -8, 300, 0).is_ok());
        assert_eq!(
            validate_price(7_500_000_000, 150_000_000, -8, 100, 0),
            Err(PriceRejection::ConfidenceTooWide)
        );
    }

    #[test]
    fn enforces_price_floor() {
        // $4.99 under a $5 floor
        assert_eq!(validate_price(499_000_000, 0, -8, 0, 5_000_000), Err(PriceRejection::BelowFloor));
        assert_eq!(validate_price(500_000_000, 0, -8, 0, 5_000_000), Ok(5_000_000));
    }

    #[test]
    fn rejects_negative_prices_like_april_2020() {
        // WTI May 2020 settled at -$37.63
        assert_eq!(validate_price(-3_763_000_000, 1_000_000, -8, 0, 0), Err(PriceRejection::Invalid));
    }

    #[test]
    fn scales_down_when_expo_below_minus_six() {
        // $75.00 with Pyth's common expo of -8
        assert_eq!(normalize_pyth_price(7_500_000_000, -8), Some(75_000_000));
    }

    #[test]
    fn scales_up_when_expo_above_minus_six() {
        // $75.00 with expo -5
        assert_eq!(normalize_pyth_price(7_500_000, -5), Some(75_000_000));
        // $75 with expo 0
        assert_eq!(normalize_pyth_price(75, 0), Some(75_000_000));
    }

    #[test]
    fn passes_through_when_expo_is_minus_six() {
        assert_eq!(normalize_pyth_price(75_000_000, -6), Some(75_000_000));
    }

    #[test]
    fn truncates_extra_precision() {
        // $75.12345678 -> $75.123456
        assert_eq!(normalize_pyth_price(7_512_345_678, -8), Some(75_123_456));
    }

    #[test]
    fn rejects_non_positive_prices() {
        assert_eq!(normalize_pyth_price(0, -8), None);
        assert_eq!(normalize_pyth_price(-1, -8), None);
    }

    #[test]
    fn rejects_prices_that_round_to_zero() {
        assert_eq!(normalize_pyth_price(99, -8), None);
        assert_eq!(normalize_pyth_price(i64::MAX, i32::MIN), None);
    }

    #[test]
    fn rejects_overflow() {
        assert_eq!(normalize_pyth_price(i64::MAX, 0), None);
        assert_eq!(normalize_pyth_price(1, i32::MAX), None);
    }
}
