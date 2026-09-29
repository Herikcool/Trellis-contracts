# Versioned domain events

Trellis domain events are public contract data. Producers should publish a
`DomainEvent` with an explicit `name` and non-zero `version`, and validate the
payload against an `EventSchema` before emitting it. Required fields form the
schema allow-list: a missing required field or an undocumented field is
rejected before publication.

Consumers advertise the versions they can decode. The compatibility rule is
to use the newest supported version that is no newer than the producer's
version. If no such version exists, the event must be rejected or routed to a
dead-letter/review path; consumers must not guess from payload shape.

Adding an optional field requires a new schema version and a decoder fallback.
Removing or changing the meaning of a field requires a new event name or a
major version. Existing consumers remain compatible because the producer's
version is carried in the event topics and payload.

The helpers and focused tests live in `shared::domain_events`.
