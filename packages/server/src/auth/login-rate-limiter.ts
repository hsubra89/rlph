import { Context, Effect, Layer } from "effect"

const WINDOW_MS = 10_000
const MAX_REQUESTS = 5

export interface LoginRateLimiterShape {
  /** Returns true if the request is allowed, false if rate-limited. */
  readonly check: (ip: string) => Effect.Effect<boolean>
}

export class LoginRateLimiter extends Context.Tag("LoginRateLimiter")<LoginRateLimiter, LoginRateLimiterShape>() {}

/** In-memory per-IP sliding window rate limiter. Swap for Redis-backed implementation to scale horizontally. */
export const LoginRateLimiterLive = Layer.sync(LoginRateLimiter, () => {
  const requests = new Map<string, number[]>()
  let lastCleanup = Date.now()

  function cleanup(now: number) {
    if (now - lastCleanup < WINDOW_MS) return
    lastCleanup = now
    const cutoff = now - WINDOW_MS
    for (const [ip, timestamps] of requests) {
      const filtered = timestamps.filter((t) => t > cutoff)
      if (filtered.length === 0) requests.delete(ip)
      else requests.set(ip, filtered)
    }
  }

  return {
    check: (ip: string) =>
      Effect.sync(() => {
        const now = Date.now()
        cleanup(now)
        const cutoff = now - WINDOW_MS
        const timestamps = (requests.get(ip) ?? []).filter((t) => t > cutoff)

        if (timestamps.length >= MAX_REQUESTS) {
          requests.set(ip, timestamps)
          return false
        }

        timestamps.push(now)
        requests.set(ip, timestamps)
        return true
      }),
  }
})
