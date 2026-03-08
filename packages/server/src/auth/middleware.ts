import { HttpServerRequest, HttpServerResponse } from "@effect/platform"
import type { HttpBodyError } from "@effect/platform/HttpBody"
import { Effect } from "effect"
import * as jose from "jose"

export interface AuthClaims {
  sub: string
  ghuser: string
}

export const makeAuthMiddleware = (jwtSecret: Uint8Array) => {
  const verify = (
    handler: Effect.Effect<
      HttpServerResponse.HttpServerResponse,
      HttpBodyError,
      HttpServerRequest.HttpServerRequest
    >,
  ): Effect.Effect<
    HttpServerResponse.HttpServerResponse,
    HttpBodyError,
    HttpServerRequest.HttpServerRequest
  > =>
    Effect.gen(function* () {
      const request = yield* HttpServerRequest.HttpServerRequest
      const authHeader = request.headers["authorization"]

      if (!authHeader || !authHeader.startsWith("Bearer ")) {
        return yield* HttpServerResponse.json(
          { error: "Missing or invalid Authorization header" },
          { status: 401 },
        )
      }

      const token = authHeader.slice(7)

      const result = yield* Effect.tryPromise({
        try: () => jose.jwtVerify(token, jwtSecret),
        catch: () => new Error("Invalid or expired token"),
      }).pipe(
        Effect.mapError(() => undefined),
        Effect.option,
      )

      if (result._tag === "None") {
        return yield* HttpServerResponse.json(
          { error: "Invalid or expired token" },
          { status: 401 },
        )
      }

      const payload = result.value.payload as { sub?: string; ghuser?: string }
      if (!payload.sub || !payload.ghuser) {
        return yield* HttpServerResponse.json(
          { error: "Invalid token claims" },
          { status: 401 },
        )
      }

      // Store claims for the handler to use
      ;(request as any)._authClaims = { sub: payload.sub, ghuser: payload.ghuser }

      return yield* handler
    })

  return verify
}

export const getAuthClaims = Effect.gen(function* () {
  const request = yield* HttpServerRequest.HttpServerRequest
  return (request as any)._authClaims as AuthClaims
})
