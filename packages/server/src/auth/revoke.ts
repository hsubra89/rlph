import { HttpServerResponse } from "@effect/platform"
import { Effect } from "effect"
import { AuthClaims } from "./middleware.js"
import { TokenDenylist } from "./token-denylist.js"

export const handleRevoke = Effect.gen(function* () {
  const { jti } = yield* AuthClaims

  const denylist = yield* TokenDenylist
  yield* denylist.revoke(jti)

  return yield* HttpServerResponse.json({ revoked: true })
})
