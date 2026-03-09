import { Clock, Context, Duration, Effect, HashMap, Layer, Ref, Schedule } from "effect"
import { TIMESTAMP_FRESHNESS_SECS } from "./constants.js"
import { pruneExpired } from "./map-utils.js"

export interface ReplayGuardShape {
  /** Returns true if this (username, timestamp) pair has already been used. */
  readonly checkAndMark: (username: string, timestamp: number) => Effect.Effect<boolean>
}

export class ReplayGuard extends Context.Tag("ReplayGuard")<ReplayGuard, ReplayGuardShape>() {}

const TTL_MS = 2 * TIMESTAMP_FRESHNESS_SECS * 1000

/** In-memory replay guard backed by a Ref<HashMap>. Swap for Redis-backed implementation to scale horizontally. */
export const ReplayGuardLive = Layer.scoped(
  ReplayGuard,
  Effect.gen(function* () {
    const ref = yield* Ref.make(HashMap.empty<string, number>())

    // Periodic bulk eviction of all expired entries so the map doesn't grow unboundedly.
    yield* Effect.forkScoped(
      Effect.repeat(
        Clock.currentTimeMillis.pipe(
          Effect.flatMap((now) => Ref.update(ref, (map) => pruneExpired(map, (expiry) => expiry <= now))),
        ),
        Schedule.fixed(Duration.millis(TTL_MS)),
      ),
    )

    return {
      checkAndMark: (username: string, timestamp: number) =>
        Effect.gen(function* () {
          const now = yield* Clock.currentTimeMillis
          const key = `${username}:${timestamp}`

          return yield* Ref.modify(ref, (map) => {
            // Lazy: only check/evict the single key being looked up, not the entire map.
            let updated = map
            const existingExpiry = HashMap.get(map, key)
            if (existingExpiry._tag === "Some" && existingExpiry.value <= now) {
              updated = HashMap.remove(map, key)
            }

            if (HashMap.has(updated, key)) {
              return [true, updated] as const
            }

            return [false, HashMap.set(updated, key, now + TTL_MS)] as const
          })
        }),
    }
  }),
)
