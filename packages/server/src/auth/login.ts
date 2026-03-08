import { HttpServerRequest, HttpServerResponse } from "@effect/platform"
import { Effect } from "effect"
import * as jose from "jose"
import { sshFingerprint, verifySshSignature } from "./ssh.js"

export const makeHandleLogin = (jwtSecret: Uint8Array) =>
  Effect.gen(function* () {
    const request = yield* HttpServerRequest.HttpServerRequest
    const body = yield* request.json as Effect.Effect<
      { username: string; fingerprint: string; timestamp: number; signature: string },
      unknown
    >

    const { username, fingerprint, timestamp, signature } = body as {
      username: string
      fingerprint: string
      timestamp: number
      signature: string
    }

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
        { error: "Timestamp too stale" },
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
    const matchingKey = ghKeys.find((key) => sshFingerprint(key) === fingerprint)

    if (!matchingKey) {
      return yield* HttpServerResponse.json(
        { error: "Fingerprint does not match any GitHub key" },
        { status: 403 },
      )
    }

    // Reconstruct payload and verify signature
    const payload = `${username}\n${fingerprint}\n${timestamp}`

    const verified = yield* Effect.tryPromise({
      try: () => verifySshSignature(matchingKey, signature, payload),
      catch: () => new Error("Signature verification failed"),
    })

    if (!verified) {
      return yield* HttpServerResponse.json(
        { error: "Signature verification failed" },
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
