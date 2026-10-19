# Conflict-aware operation replay

The shared `stale_state` module provides a durable envelope for client work
that must be retried after reconnecting. A client should persist the operation
ID, payload, and prepared `StateSnapshot` before attempting to submit it. Tabs
sharing an operation must reuse the same 32-byte ID; enqueueing the same ID and
payload is idempotent, while reusing it for different work returns
`OperationIdConflict`.

Before replay, call `check_before_replay` with the current wallet, API, and
ledger revisions. A current operation is atomically claimed as `InProgress`, so
a second tab with the same operation ID cannot claim it again. A stale operation
is stored as `Conflict` with a deterministic reason and recovery action. The
caller must ask the user to either retry against the latest snapshot with
`RetryWithLatest` or discard the operation. Mark successful work with
`complete_operation`; discarded and completed records prevent stale tabs from
submitting them again. If a claimed operation is interrupted and the ledger,
wallet, or API revision advances, its next check parks it as a conflict for
explicit recovery.

Queue records use persistent contract storage and are subject to the repository's
persistent-storage TTL policy. Clients should retain their local envelope until
they receive a terminal result and should treat an expired/missing record as a
request to refresh and re-simulate. This module supplies the conflict state
machine; browser storage and cross-tab notifications remain responsibilities of
the client application.
