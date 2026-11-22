# Rebalancer drift and gas policy

`decide_rebalance` is a read-only planning gate for callers that submit
portfolio rebalances. It applies the drift hysteresis threshold first, then
the gas budget:

- drift below `hysteresis_bps` returns `DeferredForDrift`;
- drift at or above the threshold is eligible to run;
- an eligible rebalance whose estimated gas is above `gas_budget` returns
  `DeferredForGas`;
- equality at either boundary is accepted, so repeated polling does not
  oscillate at the threshold.

Callers can retry a deferred decision after the portfolio drift or fee market
changes. The policy does not mutate balances or reserve a transaction, so a
retry is safe and existing execution paths remain unchanged.
