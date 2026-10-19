# Public service status

`shared::public_status` provides a small, on-chain status snapshot that API
servers, dashboards, and other public clients can read without exposing
operator notes, credentials, dependency URLs, or incident logs.

## Read and update

```rust
use shared::{get_public_status, set_public_status, PublicStatus, PublicStatusReason};

let status = get_public_status(&env);
set_public_status(
    &env,
    &admin,
    PublicStatus::Degraded,
    PublicStatusReason::ProcessingDelay,
)?;
```

The setter requires the configured admin's authorization. Updates with a reason
that does not match the selected state are rejected. Each successful change is
stored and emitted as a `pub_stat` event with only the fixed status enum, fixed
reason enum, and ledger sequence. No free-form text is accepted.

## Status and reason mapping

| Status | Allowed public reason codes |
| --- | --- |
| `Unknown` | Returned before an operator reports a state; cannot be set. |
| `Operational` | `Normal` |
| `Degraded` | `DependencyUnavailable`, `ProcessingDelay`, `ElevatedFailures` |
| `Paused` | `EmergencyPause`, `ScheduledPause` |
| `Maintenance` | `ScheduledMaintenance`, `UpgradeInProgress` |

Consumers should display `Unknown` as “status not reported,” rather than
assuming the system is healthy. Use `updated_at_ledger` to show freshness and
avoid treating an old snapshot as current. Once service recovers, publish
`Operational` with `Normal`; the new record replaces the old state and advances
the ledger marker.

The shared crate exposes this model for contract and off-chain integrations. A
deployment should connect its operator incident, pause, and maintenance
workflows to the authorized setter and have its API/dashboard read the snapshot.
Existing endpoints and response schemas remain unchanged until an integration
chooses to expose the new additive model.
