import { HttpServerResponse } from "@effect/platform"
import { Effect } from "effect"
import { getAuthClaims } from "./middleware.js"

export const handleWhoami = Effect.gen(function* () {
  const claims = yield* getAuthClaims
  return yield* HttpServerResponse.json({ ghuser: claims.ghuser, sub: claims.sub })
})
