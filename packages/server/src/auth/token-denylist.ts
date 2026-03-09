import { Context, Duration, Effect, Layer, Ref } from "effect"
import { JWT_LIFETIME_DURATION } from "./constants.js"

export interface TokenDenylistShape {
  /** Mark a JTI as revoked. */
  readonly revoke: (jti: string) => Effect.Effect<void>
  /** Returns true if the JTI has been revoked. */
  readonly isRevoked: (jti: string) => Effect.Effect<boolean>
}

export class TokenDenylist extends Context.Tag("TokenDenylist")<TokenDenylist, TokenDenylistShape>() {}

const ttlMs = Duration.toMillis(JWT_LIFETIME_DURATION)

/**
 * In-memory denylist backed by a `Ref<Map<jti, expiresAt>>`.
 *
 * Explicit set semantics: `revoke` inserts the JTI with an expiry timestamp
 * equal to `now + JWT_LIFETIME_DURATION`; `isRevoked` checks membership. A
 * background fiber sweeps expired entries every minute so memory stays bounded
 * without requiring an external cache primitive.
 */
export const TokenDenylistLive = Layer.scoped(
  TokenDenylist,
  Effect.gen(function* () {
    // JTI → expiry timestamp (ms since epoch)
    const store = yield* Ref.make(new Map<string, number>())

    // Background cleanup fiber: evicts expired entries every minute.
    const cleanup = Effect.gen(function* () {
      while (true) {
        yield* Effect.sleep(Duration.minutes(1))
        const now = Date.now()
        yield* Ref.update(store, (map) => {
          const next = new Map(map)
          for (const [jti, expiresAt] of next) {
            if (now >= expiresAt) next.delete(jti)
          }
          return next
        })
      }
    })
    yield* Effect.forkScoped(cleanup)

    return {
      /**
       * Marks the JTI as revoked for the remainder of its validity window.
       * The entry expires automatically after JWT_LIFETIME_DURATION, so no
       * revoked token can outlive its own validity window.
       */
      revoke: (jti: string) =>
        Ref.update(store, (map) => new Map(map).set(jti, Date.now() + ttlMs)),
      isRevoked: (jti: string) =>
        Ref.get(store).pipe(Effect.map((map) => map.has(jti))),
    }
  }),
)
