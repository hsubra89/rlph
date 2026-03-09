# Effect-TS Idiomatic Patterns

## Effect.fn vs Effect.fnUntraced vs Effect.gen

**`Effect.fn`** — use for **named, reusable functions** that return effects. Adds tracing spans automatically. Also acts as a pipe, letting you append operators after the generator body.

```ts
// Named function with automatic span
const getUser = Effect.fn("getUser")(function*(id: string) {
  const db = yield* Database
  return yield* db.findUser(id)
})

// With pipeline operators after the body
const getUser = Effect.fn("getUser")(
  function*(id: string) {
    const db = yield* Database
    return yield* db.findUser(id)
  },
  // operators receive the effect + original args
  (effect, id) => Effect.annotateLogs(effect, "userId", id)
)
```

**`Effect.fnUntraced`** — same as `Effect.fn` but **skips span creation**. Use for hot-path / internal functions where tracing overhead matters.

```ts
// Performance-sensitive internal function
const processChunk = Effect.fnUntraced(function*(chunk: Chunk<byte>) {
  const codec = yield* Codec
  return yield* codec.decode(chunk)
})
```

**`Effect.gen`** — use for **inline, anonymous effect blocks** (not reusable functions). No tracing, no function name.

```ts
// Inline block within a larger flow
const program = Effect.gen(function*() {
  const config = yield* ConfigTag
  const result = yield* someAsyncOp(config)
  return result
})
```

**Decision guide:**
- Defining a reusable function → `Effect.fn` (or `Effect.fnUntraced` for hot paths)
- Inline effect block → `Effect.gen`
- Need to pipe operators onto the result → `Effect.fn` (supports trailing operators)

Use `pipe` for single transformation chains; use `gen`/`fn` for multi-step flows. Mix freely:

```ts
Effect.gen(function*() {
  const result = yield* pipe(
    stream,
    Stream.catchAllCause(() => fallback),
    Stream.runCollect
  )
})
```

## Services — Context.Tag

```ts
// Class-based tag with static layer
class Multiplier extends Context.Tag("Multiplier")<Multiplier, number>() {
  static Live = Layer.succeed(this, 2)
}

// GenericTag for simple cases
const Port = Context.GenericTag<{ PORT: number }>("Port")

// Reference with default value (no layer needed)
class SpecialNumber extends Context.Reference<SpecialNumber>()(
  "SpecialNumber",
  { defaultValue: () => 2048 }
) {}

// Interface-based service
class RunnerStorage extends Context.Tag("@effect/cluster/RunnerStorage")<RunnerStorage, {
  readonly register: (runner: Runner) => Effect.Effect<MachineId, PersistenceError>
  readonly getRunners: Effect.Effect<Array<Runner>, PersistenceError>
}>() {}
```

## Layers — composition & lifecycle

```ts
// Merging independent layers
const combined = layerA.pipe(Layer.merge(layerB))

// Providing dependencies to a layer
const fed = upperLayer.pipe(Layer.provide(lowerLayer))

// Scoped resource as layer
const layer = Layer.scoped(
  Tag,
  Effect.acquireRelease(acquire, release)
)

// Dynamic layer from effect
Layer.unwrapEffect(Effect.gen(function*() {
  const config = yield* ConfigTag
  return SomeService.layer({ host: config.host })
}))
```

## Error handling — tagged errors

```ts
// Define with Schema.TaggedError
class NotFound extends Schema.TaggedError<NotFound>()("NotFound", {
  id: Schema.String
}) {}

// Catch by tag
effect.pipe(
  Effect.catchTag("NotFound", (e) => Effect.succeed(fallback))
)

// Wrap all causes into domain error
class ServiceDefect extends Schema.TaggedError<ServiceDefect>()("ServiceDefect", {
  cause: Schema.Defect
}) {
  static wrap<A, E, R>(effect: Effect.Effect<A, E, R>) {
    return Effect.catchAllCause(
      Effect.orDie(effect),
      (cause) => Effect.fail(new ServiceDefect({ cause: Cause.squash(cause) }))
    )
  }
}
```

## Schema — data modeling

```ts
// Class schema
class User extends Schema.Class<User>("User")({
  id: Schema.Number,
  name: Schema.String
}) {}

// TaggedRequest for RPC / request-response
class GetUser extends Schema.TaggedRequest<GetUser>()("GetUser", {
  failure: Schema.String,
  success: User,
  payload: { id: Schema.Number }
}) {}
```

## Resource management — acquireRelease & Scope

```ts
// Basic acquire/release
const resource = Effect.acquireRelease(
  openConnection(),    // acquire
  (conn) => conn.close // release (runs on scope close)
)

// Use within scoped context
Effect.scoped(
  Effect.gen(function*() {
    const conn = yield* resource
    return yield* conn.query("SELECT 1")
  })
)
```

## Stream

```ts
// Build, transform, consume
yield* pipe(
  Stream.fromIterable(items),
  Stream.mapEffect((item) => process(item)),
  Stream.take(10),
  Stream.runCollect
)

// Error recovery
Stream.catchAllCause(() => fallbackStream)

// Resource-aware stream
Stream.acquireRelease(open, close).pipe(
  Stream.flatMap((resource) => Stream.fromEffect(resource.read))
)
```

## Fiber — concurrency

```ts
// Fork background work
const fiber = yield* longRunningTask.pipe(Effect.fork)

// Fork daemon (outlives parent scope)
yield* backgroundJob.pipe(Effect.forkDaemon)

// Wait for result
const result = yield* Fiber.join(fiber)

// Cancel
yield* Fiber.interrupt(fiber)
```

## Ref — mutable state

```ts
const counter = yield* Ref.make(0)
yield* Ref.update(counter, (n) => n + 1)
const value = yield* Ref.get(counter)
```

## Effect.all / Effect.forEach — parallel composition

```ts
// Named parallel effects
const { a, b } = yield* Effect.all({
  a: fetchUsers,
  b: fetchOrders
}, { concurrency: "unbounded" })

// Parallel iteration
yield* Effect.forEach(ids, (id) => fetchById(id), { concurrency: 10 })
```

## ManagedRuntime — long-lived service context

```ts
// Create once, run many effects
const runtime = ManagedRuntime.make(appLayer)
await runtime.runPromise(myEffect)
await runtime.dispose()
```
