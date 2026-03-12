import { describe, expect, it } from "@effect/vitest"
import { Effect } from "effect"
import * as crypto from "node:crypto"
import { verifyWebhookSignature, WebhookSignatureError } from "../../src/github/webhook-signature.js"
import { signPayload } from "../helpers/webhook.js"

const SECRET = "test-webhook-secret"

describe("verifyWebhookSignature", () => {
  it.effect("accepts a valid HMAC signature", () =>
    Effect.gen(function* () {
      const body = JSON.stringify({ action: "opened" })
      const sig = signPayload(SECRET, body)
      yield* verifyWebhookSignature(SECRET, sig, body)
    }),
  )

  it.effect("rejects a tampered body", () =>
    Effect.gen(function* () {
      const body = JSON.stringify({ action: "opened" })
      const sig = signPayload(SECRET, body)
      const error = yield* verifyWebhookSignature(SECRET, sig, body + "tampered").pipe(Effect.flip)
      expect(error).toBeInstanceOf(WebhookSignatureError)
    }),
  )

  it.effect("rejects a wrong secret", () =>
    Effect.gen(function* () {
      const body = JSON.stringify({ action: "opened" })
      const sig = signPayload("wrong-secret", body)
      const error = yield* verifyWebhookSignature(SECRET, sig, body).pipe(Effect.flip)
      expect(error).toBeInstanceOf(WebhookSignatureError)
    }),
  )

  it.effect("rejects a missing sha256= prefix", () =>
    Effect.gen(function* () {
      const body = JSON.stringify({ action: "opened" })
      const hmac = crypto.createHmac("sha256", SECRET)
      hmac.update(body)
      const sigNoPrefix = hmac.digest("hex")
      const error = yield* verifyWebhookSignature(SECRET, sigNoPrefix, body).pipe(Effect.flip)
      expect(error).toBeInstanceOf(WebhookSignatureError)
    }),
  )
})
