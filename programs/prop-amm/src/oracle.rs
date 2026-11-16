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

#[cfg(test)]
mod tests {
    use super::*;

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
