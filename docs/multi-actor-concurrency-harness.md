# Multi-actor concurrency harness

The testing crate now provides a deterministic queue for actor mutation
attempts. Each queued attempt records its actor, request identifier, expected
revision, retry number, and mutation payload. A test supplies a callback that
applies each attempt to the same live contract state in the selected order.

Soroban commits ledger transactions serially. The harness therefore reproduces
the interleavings clients see around a write: multiple actors can prepare from
the same revision, a retry can arrive after the first response is lost, and an
action can become stale before execution. It does not simulate simultaneous
host execution.

The treasury scheduled-withdrawal tests cover three recovery-sensitive cases:

- Two actors prepare withdrawals from the same balance revision. After one
  succeeds, the second stale snapshot is rejected before it can overdraw.
- A retry storm replays the same scheduled action. The first attempt consumes
  the schedule and later duplicates receive NotFound.
- An expired action is rejected without changing the treasury or recipient
  balance.

Run the focused suite with cargo test -p testing concurrency. When an action
is stale or expired, preserve its request ID and contract error in the test
report. Refresh the contract state and schedule a new action identifier with a
valid time window before retrying. Do not retry a consumed action ID: it is no
longer present in contract storage.
