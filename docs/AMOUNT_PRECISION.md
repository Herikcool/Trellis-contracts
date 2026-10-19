# Amount and precision boundaries

Contract amount arithmetic uses checked `i128` operations. Zero and negative
monetary amounts are rejected by entry-point validation; overflow and underflow
return the stable `Error::Overflow` result rather than wrapping.

Basis-point calculations accept rates from `0` through `10_000`, calculate the
whole and remainder portions separately, and round down. This avoids an
intermediate multiplication overflow for values such as `i128::MAX` at a full
rate. The deterministic fuzz/property tests in
`testing/src/amount_properties.rs` cover zero, one-unit, maximum, overflow-
adjacent, and precision boundaries.
