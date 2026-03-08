import { HttpServerRequest, HttpServerResponse } from "@effect/platform"
import { Effect } from "effect"
import * as crypto from "node:crypto"
import { nonceStore } from "./nonce-store.js"

export const handleChallenge = Effect.gen(function* () {
  const request = yield* HttpServerRequest.HttpServerRequest
  const body = yield* request.json as Effect.Effect<{ pubkey: string; username: string }, unknown>

  const { pubkey, username } = body as { pubkey: string; username: string }

  if (!pubkey || !username) {
    return yield* HttpServerResponse.json({ error: "pubkey and username required" }, { status: 400 })
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

  // Normalize the submitted pubkey (strip trailing comment/email)
  const submittedKeyParts = pubkey.trim().split(/\s+/)
  const submittedKeyCore = submittedKeyParts.slice(0, 2).join(" ")

  const found = ghKeys.some((ghKey) => {
    const ghKeyParts = ghKey.split(/\s+/)
    const ghKeyCore = ghKeyParts.slice(0, 2).join(" ")
    return ghKeyCore === submittedKeyCore
  })

  if (!found) {
    return yield* HttpServerResponse.json(
      { error: "Public key not found in GitHub user's keys" },
      { status: 403 },
    )
  }

  // Generate nonce and store it
  const nonce = crypto.randomBytes(32).toString("hex")
  nonceStore.set(nonce, pubkey.trim(), username)

  return yield* HttpServerResponse.json({ nonce })
})
