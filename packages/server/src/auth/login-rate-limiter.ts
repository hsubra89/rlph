import { Chunk, Clock, Context, Duration, Effect, HashMap, Layer, Ref, Schedule } from "effect"
import { pruneExpired } from "./map-utils.js"

const WINDOW_MS = 10_000
const MAX_REQUESTS = 5

export interface LoginRateLimiterShape {
  /** Returns true if the request is allowed, false if rate-limited. */
  readonly check: (ip: string) => Effect.Effect<boolean>
}

export class LoginRateLimiter extends Context.Tag("LoginRateLimiter")<LoginRateLimiter, LoginRateLimiterShape>() {}

/** In-memory per-IP sliding window rate limiter. Swap for Redis-backed implementation to scale horizontally. */
export const LoginRateLimiterLive = Layer.effect(
  LoginRateLimiter,
  Effect.gen(function* () {
    const ref = yield* Ref.make(HashMap.empty<string, Chunk.Chunk<number>>())

    // Periodic bulk eviction of all stale entries so the map doesn't grow unboundedly.
    yield* Effect.forkDaemon(
      Effect.repeat(
        Clock.currentTimeMillis.pipe(
          Effect.flatMap((now) =>
            Ref.update(ref, (map) =>
              pruneExpired(map, (timestamps) =>
                Chunk.isEmpty(Chunk.filter(timestamps, (t) => t > now - WINDOW_MS)),
              ),
            ),
          ),
        ),
        Schedule.fixed(Duration.millis(WINDOW_MS)),
      ),
    )

    return {
      check: (ip: string) =>
        Effect.gen(function* () {
          const now = yield* Clock.currentTimeMillis

          return yield* Ref.modify(ref, (map) => {
            const cutoff = now - WINDOW_MS

            // Lazy: only filter timestamps for this IP, not the entire map.
            const existing = HashMap.get(map, ip)
            const timestamps =
              existing._tag === "Some"
                ? Chunk.filter(existing.value, (t) => t > cutoff)
                : Chunk.empty<number>()

            if (Chunk.size(timestamps) >= MAX_REQUESTS) {
              return [false, HashMap.set(map, ip, timestamps)] as const
            }

            return [true, HashMap.set(map, ip, Chunk.append(timestamps, now))] as const
          })
        }),
    }
  }),
)
