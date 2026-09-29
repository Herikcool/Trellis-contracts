# Stale state detection and recovery

An operation prepared from a wallet, API cache, or ledger snapshot must not be
submitted after any of those sources changes. `shared::stale_state::detect`
compares the prepared and current `StateSnapshot` values without reading or
mutating storage.

Ledger drift requires re-simulation; wallet drift refreshes wallet state; API
drift refreshes API data. Ledger drift takes precedence because simulation
results can be invalid even if cached wallet and API revisions still match.
Callers should run the detector before signing irreversible operations, show
the recovery action to the user, and rebuild the transaction after recovery.
The existing preflight module remains the submission guard for contract paths.
