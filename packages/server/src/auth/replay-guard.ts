import { Clock, Context, Effect, HashMap, Layer, Ref } from "effect"
import { TIMESTAMP_FRESHNESS_SECS } from "./constants.js"

export interface ReplayGuardShape {
  /** Returns true if this (username, timestamp) pair has already been used. */
  readonly checkAndMark: (username: string, timestamp: number) => Effect.Effect<boolean>
}

export class ReplayGuard extends Context.Tag("ReplayGuard")<ReplayGuard, ReplayGuardShape>() {}

const TTL_MS = TIMESTAMP_FRESHNESS_SECS * 1000

/** In-memory replay guard backed by a Ref<HashMap>. Swap for Redis-backed implementation to scale horizontally. */
export const ReplayGuardLive = Layer.effect(
  ReplayGuard,
  Effect.gen(function* () {
    const ref = yield* Ref.make(HashMap.empty<string, number>())

    return {
      checkAndMark: (username: string, timestamp: number) =>
        Effect.gen(function* () {
          const now = yield* Clock.currentTimeMillis
          const key = `${username}:${timestamp}`

          return yield* Ref.modify(ref, (map) => {
            // Evict expired entries
            let pruned = map
            for (const [k, expiry] of map) {
              if (expiry <= now) pruned = HashMap.remove(pruned, k)
            }

            if (HashMap.has(pruned, key)) {
              return [true, pruned] as const
            }

            return [false, HashMap.set(pruned, key, now + TTL_MS)] as const
          })
        }),
    }
  }),
)
