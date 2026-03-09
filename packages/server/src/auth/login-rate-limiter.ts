import { Chunk, Clock, Context, Effect, HashMap, Layer, Ref } from "effect"

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

    return {
      check: (ip: string) =>
        Effect.gen(function* () {
          const now = yield* Clock.currentTimeMillis

          return yield* Ref.modify(ref, (map) => {
            const cutoff = now - WINDOW_MS

            // Prune expired entries
            let pruned = map
            for (const [key, timestamps] of map) {
              const filtered = Chunk.filter(timestamps, (t) => t > cutoff)
              if (Chunk.isEmpty(filtered)) pruned = HashMap.remove(pruned, key)
              else pruned = HashMap.set(pruned, key, filtered)
            }

            const timestamps = HashMap.get(pruned, ip).pipe(
              (opt) => (opt._tag === "Some" ? opt.value : Chunk.empty<number>()),
            )

            if (Chunk.size(timestamps) >= MAX_REQUESTS) {
              return [false, pruned] as const
            }

            return [true, HashMap.set(pruned, ip, Chunk.append(timestamps, now))] as const
          })
        }),
    }
  }),
)
