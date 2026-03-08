import { HttpServerRequest, HttpServerResponse } from "@effect/platform"
import { Effect, Either, Schema } from "effect"
import { AuthClaims } from "./middleware.js"
import { TokenDenylist } from "./token-denylist.js"

const RevokeBody = Schema.Struct({
  jti: Schema.String,
})

export const handleRevoke = Effect.gen(function* () {
  yield* AuthClaims

  const request = yield* HttpServerRequest.HttpServerRequest
  const json = yield* request.json
  const decoded = yield* Effect.either(Schema.decodeUnknown(RevokeBody)(json))

  if (Either.isLeft(decoded)) {
    return yield* HttpServerResponse.json(
      { error: "jti is required" },
      { status: 400 },
    )
  }

  const denylist = yield* TokenDenylist
  yield* denylist.revoke(decoded.right.jti)

  return yield* HttpServerResponse.json({ revoked: true })
})
