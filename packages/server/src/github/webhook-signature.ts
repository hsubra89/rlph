import { Data, Effect } from "effect"
import * as crypto from "node:crypto"

export class WebhookSignatureError extends Data.TaggedError("WebhookSignatureError")<{
  readonly reason: string
}> { }

export const verifyWebhookSignature = (
  secret: string,
  signatureHeader: string,
  rawBody: string,
): Effect.Effect<void, WebhookSignatureError> =>
  Effect.gen(function* () {
    if (!signatureHeader.startsWith("sha256=")) {
      return yield* new WebhookSignatureError({ reason: "missing sha256= prefix" })
    }

    const hex = signatureHeader.slice(7)

    if (hex.length !== 64) {
      return yield* new WebhookSignatureError({ reason: "signature mismatch" })
    }

    const sigBytes = Buffer.from(hex, "hex")

    if (sigBytes.length !== 32) {
      return yield* new WebhookSignatureError({ reason: "signature mismatch" })
    }

    const expected = crypto.createHmac("sha256", secret).update(rawBody).digest()

    if (!crypto.timingSafeEqual(sigBytes, expected)) {
      return yield* new WebhookSignatureError({ reason: "signature mismatch" })
    }
  })
