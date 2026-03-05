# Engineering Checklist

Use this checklist to keep `rlph` quality aligned with mature Rust projects (for example Tokio-style rigor around async safety and testing).

## CI Gates

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo nextest run`
- [ ] `cargo nextest run --profile integration -E 'binary(cli_binary)'`
- [ ] Optional local/full integration sweep (not merge-blocking): `cargo nextest run --profile integration`

## Design And API

- [ ] Keep public API surface minimal (`pub(crate)` by default).
- [ ] Separate pure domain logic from runtime/IO glue.
- [ ] Use additive feature flags and keep defaults lean.
- [ ] Prefer enums/newtypes over stringly-typed state.
- [ ] Avoid panics in non-test code; return structured errors.

## Async And Concurrency

- [ ] Never block async executors; use `tokio::task::spawn_blocking` for blocking work.
- [ ] Design for cancellation safety and bounded resource usage.
- [ ] Use bounded channels/timeouts where backpressure matters.
- [ ] Keep `unsafe` blocks minimal, documented, and invariants explicit.

## Testing

- [ ] Follow red-green-refactor (TDD) for feature and bug work.
- [ ] Cover happy path, failure path, and cancellation behavior.
- [ ] Keep async tests deterministic (avoid fragile sleep-based timing).
- [ ] Add targeted race/concurrency tests for critical paths.

## Observability And Ops

- [ ] Add `tracing` spans/events around key pipeline steps.
- [ ] Document module responsibilities and invariants.
- [ ] Keep versioning/release behavior semver-consistent.
