import { HttpServerRequest, HttpServerResponse } from "@effect/platform"
import { Effect, Either } from "effect"
import * as jose from "jose"
import { sshFingerprint, verifySshSignature } from "./ssh.js"

interface LoginBody {
  username: string
  fingerprint: string
  timestamp: number
  signature: string
}

export const makeHandleLogin = (jwtSecret: Uint8Array) =>
  Effect.gen(function* () {
    const request = yield* HttpServerRequest.HttpServerRequest
    const { username, fingerprint, timestamp, signature } =
      (yield* request.json) as LoginBody

    if (!username || !fingerprint || !timestamp || !signature) {
      return yield* HttpServerResponse.json(
        { error: "username, fingerprint, timestamp, and signature required" },
        { status: 400 },
      )
    }

    // Check timestamp freshness (60s window)
    const now = Math.floor(Date.now() / 1000)
    if (Math.abs(now - timestamp) > 60) {
      return yield* HttpServerResponse.json(
        { error: "timestamp too stale" },
        { status: 401 },
      )
    }

    // Fetch GitHub public keys for the user
    const ghKeysResponse = yield* Effect.tryPromise({
      try: () => fetch(`https://github.com/${encodeURIComponent(username)}.keys`),
      catch: () => new Error("Failed to fetch GitHub keys"),
    })

    if (!ghKeysResponse.ok) {
      return yield* HttpServerResponse.json(
        { error: `GitHub user '${username}' not found` },
        { status: 403 },
      )
    }

    const ghKeysText = yield* Effect.tryPromise({
      try: () => ghKeysResponse.text(),
      catch: () => new Error("Failed to read GitHub keys response"),
    })

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
        { error: `signature verification failed: ${verifyResult.left.reason}` },
        { status: 401 },
      )
    }

    // Issue JWT
    const token = yield* Effect.tryPromise({
      try: () =>
        new jose.SignJWT({ ghuser: username })
          .setProtectedHeader({ alg: "HS256" })
          .setSubject(fingerprint)
          .setIssuedAt()
          .setExpirationTime("1h")
          .sign(jwtSecret),
      catch: (e) => new Error(`JWT signing failed: ${e}`),
    })

    return yield* HttpServerResponse.json({ token })
  })
