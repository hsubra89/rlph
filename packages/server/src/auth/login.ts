import { HttpClient, HttpServerRequest, HttpServerResponse } from "@effect/platform"
import { Data, Effect, Either, Option, Schema } from "effect"
import * as crypto from "node:crypto"
import * as jose from "jose"
import { LoginRateLimiter } from "./login-rate-limiter.js"
import { ReplayGuard } from "./replay-guard.js"
import { sshFingerprint, verifySshSignature } from "./ssh.js"

export class JwtSignError extends Data.TaggedError("JwtSignError")<{
  readonly cause: unknown
}> { }

const LoginBody = Schema.Struct({
  username: Schema.String,
  fingerprint: Schema.String,
  timestamp: Schema.Number,
  signature: Schema.String,
})

export const makeHandleLogin = (jwtSecret: Uint8Array) =>
  Effect.gen(function* () {
    const request = yield* HttpServerRequest.HttpServerRequest

    // Rate limit by client IP before any further processing
    const rateLimiter = yield* LoginRateLimiter
    const ip = Option.getOrElse(request.remoteAddress, () => "unknown")
    const allowed = yield* rateLimiter.check(ip)
    if (!allowed) {
      return yield* HttpServerResponse.json(
        { error: "too many requests" },
        { status: 429 },
      )
    }

    const json = yield* request.json
    const decoded = yield* Effect.either(
      Schema.decodeUnknown(LoginBody)(json),
    )

    if (Either.isLeft(decoded)) {
      return yield* HttpServerResponse.json(
        { error: "username, fingerprint, timestamp, and signature required" },
        { status: 400 },
      )
    }

    const { username, fingerprint, timestamp, signature } = decoded.right

    // Check timestamp freshness (60s window)
    const now = Math.floor(Date.now() / 1000)

    if (Math.abs(now - timestamp) > 60) {
      return yield* HttpServerResponse.json(
        { error: "timestamp too stale" },
        { status: 401 },
      )
    }

    // Reject replayed (username, timestamp) pairs within the freshness window
    const replayGuard = yield* ReplayGuard
    const isReplay = yield* replayGuard.checkAndMark(username, timestamp)
    if (isReplay) {
      return yield* HttpServerResponse.json(
        { error: "duplicate request" },
        { status: 401 },
      )
    }

    // Fetch GitHub public keys for the user
    const ghKeysResponse = yield* Effect.either(
      HttpClient.get(`https://github.com/${encodeURIComponent(username)}.keys`),
    )

    if (Either.isLeft(ghKeysResponse) || ghKeysResponse.right.status !== 200) {
      return yield* HttpServerResponse.json(
        { error: `GitHub user '${username}' not found` },
        { status: 403 },
      )
    }

    const ghKeysText = yield* ghKeysResponse.right.text

    const ghKeys = ghKeysText
      .split("\n")
      .map((k) => k.trim())
      .filter((k) => k.length > 0)

    // Find the key matching the submitted fingerprint
    const matchingKey = ghKeys.find((key) => {
      const fp = sshFingerprint(key)
      return Either.isRight(fp) && fp.right === fingerprint
    })

    if (!matchingKey) {
      return yield* HttpServerResponse.json(
        { error: "fingerprint does not match any GitHub key" },
        { status: 403 },
      )
    }

    // Reconstruct payload and verify signature
    const payload = `${username}\n${fingerprint}\n${timestamp}`

    const verifyResult = yield* Effect.either(
      verifySshSignature(matchingKey, signature, payload),
    )

    if (Either.isLeft(verifyResult)) {
      return yield* HttpServerResponse.json(
        { error: "signature verification failed" },
        { status: 401 },
      )
    }

    // Issue JWT with unique JTI for revocation support
    const jti = crypto.randomUUID()
    const token = yield* Effect.tryPromise({
      try: () =>
        new jose.SignJWT({ ghuser: username })
          .setProtectedHeader({ alg: "HS256" })
          .setSubject(fingerprint)
          .setJti(jti)
          .setIssuedAt()
          .setExpirationTime("1h")
          .sign(jwtSecret),
      catch: (cause) => new JwtSignError({ cause }),
    })

    return yield* HttpServerResponse.json({ token })
  })
