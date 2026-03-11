import { HttpServerRequest, HttpServerResponse } from "@effect/platform"
import { Context, Data, Effect, Either } from "effect"
import * as jose from "jose"
import { JwtSecret } from "../config.js"
import { TokenDenylist } from "./token-denylist.js"

export class AuthClaims extends Context.Tag("AuthClaims")<
  AuthClaims,
  {
    readonly sub: string
    readonly ghuser: string
    readonly jti: string
  }
>() {}

export class JwtVerifyError extends Data.TaggedError("JwtVerifyError")<{
  readonly reason: "invalid_token"
}> {}

export const authMiddleware = <E, R>(
  handler: Effect.Effect<HttpServerResponse.HttpServerResponse, E, R | AuthClaims>,
) =>
  Effect.gen(function* () {
    const jwtSecret = yield* JwtSecret
    const request = yield* HttpServerRequest.HttpServerRequest
    const authHeader = request.headers["authorization"]

    if (!authHeader || !authHeader.startsWith("Bearer ")) {
      return yield* HttpServerResponse.json(
        { error: "missing or invalid authorization header" },
        { status: 401 },
      )
    }

    const token = authHeader.slice(7)

    const verifyResult = yield* Effect.either(
      Effect.tryPromise({
        try: () => jose.jwtVerify(token, jwtSecret, { algorithms: ["HS256"] }),
        catch: () => new JwtVerifyError({ reason: "invalid_token" }),
      }),
    )

    if (Either.isLeft(verifyResult)) {
      return yield* HttpServerResponse.json({ error: "invalid or expired token" }, { status: 401 })
    }

    const { payload } = verifyResult.right
    if (
      typeof payload.sub !== "string" ||
      typeof payload["ghuser"] !== "string" ||
      typeof payload.jti !== "string"
    ) {
      return yield* HttpServerResponse.json({ error: "invalid token claims" }, { status: 401 })
    }
    const claims = { sub: payload.sub, ghuser: payload["ghuser"], jti: payload.jti }

    const denylist = yield* TokenDenylist
    const revoked = yield* denylist.isRevoked(claims.jti)
    if (revoked) {
      return yield* HttpServerResponse.json({ error: "token has been revoked" }, { status: 401 })
    }

    return yield* handler.pipe(Effect.provideService(AuthClaims, claims))
  })
