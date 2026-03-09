# crates/brrr-core

Pure domain types and algorithms. **Zero IO** — no network, no filesystem, no process spawning. Depends only on `serde`, `regex`, `tracing`.

## Modules

| Module | What |
|--------|------|
| `ids.rs` | Newtype wrappers: `IssueNumber`, `PrNumber`, `CommentId`, `ReactionId`. Macro-based, serde-transparent, `FromStr`/`Display`. |
| `task.rs` | `Task` struct (id, title, body, labels, url, priority). `Priority` enum (1–9 + named variants). |
| `deps.rs` | `DependencyGraph`: parses "blocked by #N", "depends on #N", "blockedBy: [N, M]" from task bodies. Eligibility filtering. |
| `scc.rs` | Tarjan's Strongly Connected Components for cycle detection in dependency graphs. |

## Constraints

- No IO. If it touches the network or filesystem, it belongs in `crates/brrr/`.
- Prefer enums/newtypes over stringly-typed state.
- Return structured errors — no panics, no `.unwrap()`.

## Re-exports

The `brrr` crate re-exports `ids`, `scc`, `deps`, `task` so existing `crate::` paths work.

## Testing

Unit tests in-module. Run with `cargo nextest run -p brrr-core`.
