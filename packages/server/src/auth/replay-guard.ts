import { Cache, Context, Effect, Layer } from "effect"

export interface ReplayGuardShape {
  /** Returns true if this (username, timestamp) pair has already been used. */
  readonly checkAndMark: (username: string, timestamp: number) => Effect.Effect<boolean>
}

export class ReplayGuard extends Context.Tag("ReplayGuard")<ReplayGuard, ReplayGuardShape>() {}

/** In-memory replay guard backed by Effect Cache with TTL. Swap for Redis-backed implementation to scale horizontally. */
export const ReplayGuardLive = Layer.effect(
  ReplayGuard,
  Effect.gen(function* () {
    const cache = yield* Cache.make({
      capacity: 10_000,
      timeToLive: "60 seconds",
      lookup: (_: string) => Effect.void,
    })

    return {
      checkAndMark: (username: string, timestamp: number) =>
        Effect.gen(function* () {
          const key = `${username}:${timestamp}`
          const seen = yield* cache.contains(key)
          if (!seen) yield* cache.get(key)
          return seen
        }),
    }
  }),
)
