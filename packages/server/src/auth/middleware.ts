import { HttpServerRequest, HttpServerResponse } from "@effect/platform"
import { Context, Data, Effect, Either } from "effect"
import * as jose from "jose"
import { AppConfigTag } from "../config.js"
import { TokenDenylist } from "./token-denylist.js"

export class AuthClaims extends Context.Tag("AuthClaims")<AuthClaims, {
  readonly sub: string
  readonly ghuser: string
}>() {}

export class JwtVerifyError extends Data.TaggedError("JwtVerifyError")<{
  readonly reason: "invalid_token" | "invalid_claims"
}> {}

export const authMiddleware =
  <E, R>(handler: Effect.Effect<HttpServerResponse.HttpServerResponse, E, R | AuthClaims>) =>
    Effect.gen(function* () {
      const { jwtSecret } = yield* AppConfigTag
      const request = yield* HttpServerRequest.HttpServerRequest
      const authHeader = request.headers["authorization"]

      if (!authHeader || !authHeader.startsWith("Bearer ")) {
        return yield* HttpServerResponse.json(
          { error: "Missing or invalid Authorization header" },
          { status: 401 },
        )
      }

      const token = authHeader.slice(7)

      const verifyResult = yield* Effect.either(
        Effect.tryPromise({
          try: () => jose.jwtVerify(token, jwtSecret),
          catch: () => new JwtVerifyError({ reason: "invalid_token" }),
        }),
      )

      if (Either.isLeft(verifyResult)) {
        return yield* HttpServerResponse.json(
          { error: "Invalid or expired token" },
          { status: 401 },
        )
      }

      const payload = verifyResult.right.payload as { sub?: string; ghuser?: string; jti?: string }
      if (!payload.sub || !payload.ghuser) {
        return yield* HttpServerResponse.json(
          { error: "Invalid token claims" },
          { status: 401 },
        )
      }

      if (payload.jti) {
        const denylist = yield* TokenDenylist
        const revoked = yield* denylist.isRevoked(payload.jti)
        if (revoked) {
          return yield* HttpServerResponse.json(
            { error: "Token has been revoked" },
            { status: 401 },
          )
        }
      }

      return yield* handler.pipe(
        Effect.provideService(AuthClaims, { sub: payload.sub, ghuser: payload.ghuser }),
      )
    })
