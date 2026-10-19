## Description

Please include a summary of the changes and the related issue. Please also include relevant motivation and context. See [CONTRIBUTING.md](../CONTRIBUTING.md) before opening a PR.

Fixes #(issue number)

**Affected crate(s):**

## Type of Change

- [ ] Bug fix (non-breaking change which fixes an issue)
- [ ] New feature (non-breaking change which adds functionality)
- [ ] Breaking change (changes a contract function, event, or storage layout)
- [ ] Documentation update
- [ ] Performance improvement (e.g. wasm size / resource usage)
- [ ] Code refactoring (no functional changes)

## Checklist

Each item below is enforced by a job in `.github/workflows/ci.yml`.

- [ ] `cargo build --release` succeeds
- [ ] Tests added or updated, and `cargo test --workspace` passes
- [ ] `cargo fmt --all -- --check` is clean
- [ ] `cargo clippy --workspace -- -D warnings` is clean
- [ ] `./scripts/security/run-audit.sh --skip-wasm` passes (any new allowlist entry has a rationale and an expiry)
- [ ] Docs updated (README / crate docs / SECURITY.md) where behaviour changed

## On-chain Interface

- [ ] No emitted event topic or payload changed. If one did, explain why and add a migration note.
- [ ] No storage layout change. If there is one, describe the migration.

## Release Readiness Gate

Each item below is enforced by `.github/workflows/release-readiness.yml` and `scripts/release-readiness-gate.cjs`.

- [ ] Migrations: Storage schema dry-run and compatibility verified (`docs/MIGRATION_SAFETY.md`)
- [ ] Security: Security audit and invariant checks passed (`scripts/security/run-audit.sh`, `docs/SECURITY_INVARIANTS.md`)
- [ ] Smoke tests: Cross-contract wiring and getter liveness verified (`scripts/verify-deployment.sh` / `scripts/verify-deployment.cjs`)
- [ ] Rollback rehearsal: Rollback rehearsal passed and recovery steps documented (`docs/RUNBOOK.md`, `docs/RELEASE_READINESS.md`)

## Additional Notes

Add any other information that's important for reviewers to understand this PR.
