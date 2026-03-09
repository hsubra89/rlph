import { Cache, Context, Effect, Layer } from "effect"
import { JWT_LIFETIME_DURATION } from "./constants.js"

export interface TokenDenylistShape {
  /** Mark a JTI as revoked. */
  readonly revoke: (jti: string) => Effect.Effect<void>
  /** Returns true if the JTI has been revoked. */
  readonly isRevoked: (jti: string) => Effect.Effect<boolean>
}

export class TokenDenylist extends Context.Tag("TokenDenylist")<TokenDenylist, TokenDenylistShape>() {}

/** In-memory denylist backed by Effect Cache. TTL matches max JWT lifetime (1h). */
export const TokenDenylistLive = Layer.effect(
  TokenDenylist,
  Effect.gen(function* () {
    const cache = yield* Cache.make({
      capacity: 50_000,
      timeToLive: JWT_LIFETIME_DURATION,
      lookup: (_: string) => Effect.void,
    })

    return {
      revoke: (jti: string) => cache.get(jti),
      isRevoked: (jti: string) => cache.contains(jti),
    }
  }),
)
