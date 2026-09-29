//! Deterministic property checks for monetary boundaries (issue #145).

use shared::math::{apply_bps, checked_add, checked_div, checked_mul, checked_sub};

#[test]
fn checked_arithmetic_matches_i128_for_boundary_and_fuzz_values() {
    let mut state = 0x9e37_79b9_u128;
    for _ in 0..2_048 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let a = state as i128;
        state = state.rotate_left(17);
        let b = state as i128;
        assert_eq!(checked_add(a, b), a.checked_add(b).ok_or(shared::Error::Overflow));
        assert_eq!(checked_sub(a, b), a.checked_sub(b).ok_or(shared::Error::Overflow));
        assert_eq!(checked_mul(a, b), a.checked_mul(b).ok_or(shared::Error::Overflow));
        let expected = if b == 0 { Err(shared::Error::InvalidAmount) } else { a.checked_div(b).ok_or(shared::Error::Overflow) };
        assert_eq!(checked_div(a, b), expected);
    }
}

#[test]
fn basis_point_calculation_is_bounded_and_rounds_down() {
    let amounts = [1_i128, 2, 9_999, 10_000, 10_001, i128::MAX / 2, i128::MAX];
    let rates = [0_i128, 1, 99, 5_000, 9_999, 10_000];
    for amount in amounts {
        for rate in rates {
            let result = apply_bps(amount, rate).unwrap();
            assert!(result >= 0 && result <= amount);
            assert_eq!(result, amount / 10_000 * rate + (amount % 10_000 * rate) / 10_000);
        }
    }
}
