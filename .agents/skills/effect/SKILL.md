---
name: effect
description: Guide for writing idiomatic Effect-TS code. Use when code imports from "effect", "@effect/*", or user asks about Effect patterns, Effect APIs, Effect services, layers, streams, or functional effect systems in TypeScript.
---

# Effect-TS

## Source reference

The Effect source code lives at `source-references/effect/`. **Always consult it** before writing Effect code — never rely on training data alone.

## Looking up APIs

1. **Find the module** — core modules live in `source-references/effect/packages/effect/src/` (e.g., `Effect.ts`, `Stream.ts`, `Layer.ts`, `Schema.ts`).
2. **Read the source** — check function signatures, overloads, and JSDoc examples directly from the source files.
3. **Check tests for usage patterns** — tests live in `source-references/effect/packages/effect/test/` and show idiomatic usage.
4. **Other packages** — platform, SQL, RPC, CLI, cluster, etc. are under `source-references/effect/packages/<name>/`.

## Workflow

- [ ] Identify which Effect modules are relevant
- [ ] Read the source files for those modules to confirm API signatures
- [ ] Check corresponding test files for idiomatic patterns
- [ ] Write code that matches the patterns found in the source

## Key packages

| Package | Path | Use for |
|---------|------|---------|
| effect | `packages/effect/` | Core: Effect, Layer, Stream, Schema, Fiber, RateLimiter, etc. |
| platform | `packages/platform/` | HTTP client/server, filesystem, terminal |
| platform-node | `packages/platform-node/` | Node.js platform implementation |
| sql-pg | `packages/sql-pg/` | Postgres via Effect SQL |
| cli | `packages/cli/` | CLI argument parsing |
| rpc | `packages/rpc/` | RPC protocol |
| cluster | `packages/cluster/` | Entities, sharding, singleton, message routing, runners |
| experimental | `packages/experimental/` | Machine, EventLog, Persistence, PersistedQueue, DevTools |

## Idiomatic patterns

See [PATTERNS.md](PATTERNS.md) for detailed examples covering:

- `Effect.fn` vs `Effect.fnUntraced` vs `Effect.gen` — when to use each
- Services via `Context.Tag` / `Context.Reference`
- Layer composition — `merge`, `provide`, `scoped`, `unwrapEffect`
- Tagged errors with `Schema.TaggedError` + `catchTag`
- Schema class definitions and `TaggedRequest`
- Resource management with `acquireRelease` + `Scope` or `addFinalizer`.
- Stream building, transformation, error recovery
- Fiber forking, joining, interruption
- Ref for mutable state
- `Effect.all` / `Effect.forEach` for parallel composition
- `ManagedRuntime` for long-lived service contexts

## Rules

- **Never guess APIs** — if unsure, read the source file first.
- **Follow patterns from tests** — the test suite is the best style guide.
- **Use `Effect.gen`** — prefer generator syntax over pipe chains for complex flows.
- **Use `Effect.fn`** or `Effect.fnUntraced` — to define an effectful function. Use `fnUntraced` for performance sensitive parts of the application or for library code.
- **Use `Schema` for validation**.
- **Use `SqlSchema` when querying data from the databse.
